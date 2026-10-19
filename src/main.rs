mod mock;
mod state;

use std::io::Write as IoWrite;
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{json, Value};
use tiny_http::{Header, Method, Request, Response, ResponseBox, Server};

use state::{AppState, Provider};

fn main() {
    // ---- args ----------------------------------------------------------------
    let mut port: u16 = 3080;
    let mut bind: String = "127.0.0.1".to_string();
    let mut insecure = false;
    let args: Vec<String> = std::env::args().collect();
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--port" if i + 1 < args.len() => {
                port = args[i + 1].parse().unwrap_or(3080);
                i += 2;
            }
            "--bind" if i + 1 < args.len() => {
                bind = args[i + 1].clone();
                i += 2;
            }
            "--insecure" => {
                insecure = true;
                i += 1;
            }
            "-h" | "--help" => {
                println!(
                    "harness-webui-rust — DeepSeek Harness style Web UI (mock engine)\n \
                     --port <N>    listen port (default 3080)\n \
                     --bind <IP>   bind address (default 127.0.0.1)\n \
                     --insecure    allow non-loopback bind (default refused)"
                );
                return;
            }
            _ => i += 1,
        }
    }
    if !insecure && (bind == "0.0.0.0" || bind == "::" || bind == "[::]") {
        eprintln!("refusing to bind {bind} (non-loopback). Re-run with --insecure to override.");
        std::process::exit(2);
    }

    // ---- persistent state ------------------------------------------------------
    let home = std::env::var("HOME").unwrap_or_else(|_| "/root".to_string());
    let dir = PathBuf::from(&home).join(".dsh-webui-rust");
    std::fs::create_dir_all(&dir).ok();
    let state_path = dir.join("state.json");
    let settings_path = dir.join("settings.json");

    let (token, fresh_token) = match std::fs::read_to_string(&state_path) {
        Ok(s) => match serde_json::from_str::<Value>(&s).ok() {
            Some(v) => v
                .get("token")
                .and_then(|t| t.as_str())
                .map(|t| (t.to_string(), false))
                .unwrap_or_else(|| (rand_token(), true)),
            None => (rand_token(), true),
        },
        Err(_) => (rand_token(), true),
    };
    if fresh_token {
        let _ = std::fs::write(
            &state_path,
            serde_json::to_string_pretty(&json!({ "token": token })).unwrap_or_default(),
        );
    }

    // settings
    let mut app = AppState::new(settings_path.to_string_lossy().into_owned(), token.clone());
    if let Ok(s) = std::fs::read_to_string(&settings_path) {
        if let Ok(st) = serde_json::from_str::<state::Settings>(&s) {
            app.settings = st;
        }
    }
    let state: Arc<Mutex<AppState>> = Arc::new(Mutex::new(app));

    // workspace root = the directory dsh was launched from
    let root = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let root = Arc::new(root.canonicalize().unwrap_or(root));

    let addr = format!("{bind}:{port}");
    let server = Server::http(&addr).unwrap_or_else(|e| {
        eprintln!("bind {addr} failed: {e}");
        std::process::exit(1);
    });
    let launch_url = format!("http://127.0.0.1:{port}/?token={token}");
    println!("DeepSeek Harness Web UI (mock) listening on {addr}");
    println!("open:  {launch_url}");
    println!("workspace root: {}", root.display());
    if !insecure {
        println!("remote access refused (loopback only). Re-run with --bind 0.0.0.0 --insecure to expose.");
    }

    let loop_state = Arc::clone(&state);
    let loop_root = Arc::clone(&root);
    for request in server.incoming_requests() {
        // one thread per connection: a slow/SSE client can never starve the accept loop
        let state = Arc::clone(&loop_state);
        let root = Arc::clone(&loop_root);
        match std::thread::Builder::new().spawn(move || handle(request, &state, &root)) {
            Ok(_) => {}
            Err(e) => {
                // spawn failed (resource exhaustion): the failed closure dropped
                // the request, and Request::Drop sends a 500 for it, so the
                // client gets an error instead of hanging; the accept loop lives on
                eprintln!("failed to spawn handler thread ({e}); request dropped (500 sent)");
            }
        }
    }
}

fn rand_token() -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let pid = std::process::id();
    let mut hasher = DefaultHasher::new();
    (nanos, pid).hash(&mut hasher);
    format!("{:016x}{:016x}", nanos, hasher.finish())
}

fn handle(mut request: Request, state: &Arc<Mutex<AppState>>, root: &Arc<PathBuf>) {
    let method = request.method().clone();
    let url = request.url().to_string();
    let (path, query) = match url.find('?') {
        Some(n) => (&url[..n], &url[n + 1..]),
        None => (url.as_str(), ""),
    };
    let path = path.trim_start_matches('/');

    // SSE: dedicated streaming path. tiny_http's respond() funnels the body
    // through a 1024-byte BufWriter that is only flushed at end-of-stream,
    // so a long-lived stream would deliver 0 bytes to the client until it
    // finishes. Instead we take the raw socket writer and push + flush after
    // every event batch, so events reach the client in real time.
    if method == Method::Get && path == "api/stream" {
        handle_sse(request, state, query);
        return;
    }

    let mut body = String::new();
    request.as_reader().read_to_string(&mut body).ok();
    let body: Value = if body.is_empty() {
        Value::Null
    } else {
        serde_json::from_str(&body).unwrap_or(Value::Null)
    };

    let response = route(&method, path, query, &body, state, root, request.headers());
    let _ = request.respond(response);
}

/// SSE streaming: write the HTTP response header to the raw socket writer,
/// then push each drained event batch and flush immediately, so events reach
/// the client in real time (tiny_http's respond()/BufWriter only flushes at
/// end-of-stream, which would deliver 0 bytes during the stream).
fn handle_sse(request: Request, state: &Arc<Mutex<AppState>>, query: &str) {
    if !authed(request.headers(), query, state) {
        let _ = request.respond(json_error(401, "unauthenticated"));
        return;
    }

    let sid: u64 = q_param(query, "s").and_then(|v| v.parse().ok()).unwrap_or(0);
    let start_offset = {
        let st = state.lock().unwrap();
        st.sessions
            .iter()
            .find(|s| s.id == sid)
            .map(|s| s.events.len())
            .unwrap_or(0)
    };
    eprintln!("sse[{}] stream opened (offset {})", sid, start_offset);

    let http_version = request.http_version().clone();
    let mut writer = request.into_writer();

    // No Content-Length is possible for a live stream; tell clients to read
    // until the connection closes.
    let header = format!(
        "HTTP/{}.{} 200 OK\r\n\
         Content-Type: text/event-stream\r\n\
         Cache-Control: no-cache\r\n\
         X-Accel-Buffering: no\r\n\
         Connection: close\r\n\
         \r\n",
        http_version.0, http_version.1
    );
    if !write_and_flush(&mut writer, header.as_bytes()) {
        eprintln!("sse[{}] client gone before header", sid);
        return;
    }

    let mut offset = start_offset;
    let mut sent_end = false;
    loop {
        // drain new events; keep the lock held only for the drain itself
        let (evts, running) = {
            let st = state.lock().unwrap();
            match st.sessions.iter().find(|s| s.id == sid) {
                Some(s) => {
                    let new = s
                        .events
                        .get(offset..)
                        .map(|v| v.to_vec())
                        .unwrap_or_default();
                    offset += new.len();
                    (new, s.running)
                }
                None => (Vec::new(), false),
            }
        };

        let mut payload = String::new();
        for e in &evts {
            payload.push_str(&format!(
                "data: {}\n\n",
                serde_json::to_string(e).unwrap_or_default()
            ));
        }
        if !evts.is_empty() {
            eprintln!(
                "sse[{}] drained {} events (offset {}, running={})",
                sid,
                evts.len(),
                offset,
                running
            );
        }
        if !running && evts.is_empty() && !sent_end {
            payload.push_str("event: end\ndata: {}\n\n");
            sent_end = true;
        }
        if !payload.is_empty() {
            // flush after every batch: this is what makes the stream real-time
            if !write_and_flush(&mut writer, payload.as_bytes()) {
                eprintln!("sse[{}] client gone, stopping stream", sid);
                return;
            }
        }
        if sent_end && !running {
            eprintln!("sse[{}] stream EOF", sid);
            break;
        }
        std::thread::sleep(Duration::from_millis(150));
    }
    // dropping the writer flushes any remainder and closes the write side
}

fn write_and_flush(w: &mut Box<dyn IoWrite + Send>, bytes: &[u8]) -> bool {
    w.write_all(bytes).is_ok() && w.flush().is_ok()
}

fn q_param(query: &str, key: &str) -> Option<String> {
    for pair in query.split('&') {
        let mut it = pair.splitn(2, '=');
        let k = it.next()?;
        if k == key {
            return it.next().map(|v| percent_decode(v));
        }
    }
    None
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or("");
            if let Ok(v) = u8::from_str_radix(hex, 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn h(field: &str, value: &str) -> Header {
    Header::from_bytes(field.as_bytes(), value.as_bytes()).unwrap()
}

fn json_response(v: Value) -> ResponseBox {
    let b = serde_json::to_string(&v).unwrap_or_else(|_| "{}".into());
    Response::from_string(b)
        .with_header(h("Content-Type", "application/json"))
        .boxed()
}

fn json_error(code: u16, msg: &str) -> ResponseBox {
    Response::from_string(serde_json::to_string(&json!({ "error": msg })).unwrap())
        .with_status_code(code)
        .with_header(h("Content-Type", "application/json"))
        .boxed()
}

fn authed(request_headers: &[Header], query: &str, state: &Arc<Mutex<AppState>>) -> bool {
    let token = {
        let st = state.lock().unwrap();
        st.token.clone()
    };
    let cookie_ok = request_headers.iter().any(|hd| {
        hd.field.equiv("cookie")
            && hd.value.to_string().contains(&format!("dsh={token}"))
    });
    if cookie_ok {
        return true;
    }
    q_param(query, "token").as_deref() == Some(token.as_str())
}

fn safe_join(root: &std::path::Path, rel: &str) -> Option<PathBuf> {
    if rel.is_empty() {
        return Some(root.to_path_buf());
    }
    let mut stack: Vec<&str> = Vec::new();
    for seg in rel.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                if stack.pop().is_none() {
                    return None; // tried to escape root
                }
            }
            s => stack.push(s),
        }
    }
    Some(root.join(stack.join("/")))
}

fn route(
    method: &Method,
    path: &str,
    query: &str,
    body: &Value,
    state: &Arc<Mutex<AppState>>,
    root: &Arc<PathBuf>,
    req_headers: &[Header],
) -> ResponseBox {
    // ---- static / auth ------------------------------------------------------
    if path.is_empty() {
        let token = {
            let st = state.lock().unwrap();
            st.token.clone()
        };
        if let Some(t) = q_param(query, "token") {
            if t == token {
                let html = include_str!("../web/index.html").to_string();
                let mut r =
                    Response::from_string(html).with_header(h("Content-Type", "text/html; charset=utf-8"));
                r = r.with_header(h("Set-Cookie", &format!("dsh={token}; Path=/; HttpOnly")));
                return r.boxed();
            }
        }
        if authed(req_headers, query, state) {
            let html = include_str!("../web/index.html").to_string();
            return Response::from_string(html)
                .with_header(h("Content-Type", "text/html; charset=utf-8"))
                .boxed();
        }
        return json_error(401, "missing or bad token; open the launch URL printed at server start");
    }
    if path == "app.js" {
        return Response::from_string(include_str!("../web/app.js").to_string())
            .with_header(h("Content-Type", "application/javascript; charset=utf-8"))
            .boxed();
    }
    if path == "style.css" {
        return Response::from_string(include_str!("../web/style.css").to_string())
            .with_header(h("Content-Type", "text/css; charset=utf-8"))
            .boxed();
    }
    if !path.starts_with("api/") {
        return json_error(404, "not found");
    }
    if !authed(req_headers, query, state) {
        return json_error(401, "unauthenticated");
    }

    // ---- api ----------------------------------------------------------------
    match (method, path) {
        // sessions
        (Method::Get, "api/sessions") => {
            let st = state.lock().unwrap();
            let list: Vec<Value> = st
                .sessions
                .iter()
                .map(|s| serde_json::to_value(s.view()).unwrap_or_default())
                .collect();
            json_response(Value::Array(list))
        }
        (Method::Post, "api/sessions") => {
            let title = body
                .get("title")
                .and_then(|v| v.as_str())
                .unwrap_or("新会话")
                .to_string();
            let sid = {
                let mut st = state.lock().unwrap();
                let s = st.create_session(title);
                s.id
            };
            json_response(json!({ "id": sid }))
        }
        (Method::Get, "api/session") => {
            let sid: u64 = q_param(query, "s").and_then(|v| v.parse().ok()).unwrap_or(0);
            let st = state.lock().unwrap();
            match st.sessions.iter().find(|s| s.id == sid) {
                Some(s) => {
                    let events: Vec<Value> = s
                        .events
                        .iter()
                        .map(|e| serde_json::to_value(e).unwrap_or_default())
                        .collect();
                    json_response(json!({
                        "id": s.id, "title": s.title, "running": s.running,
                        "finished": s.finished, "workspace": s.workspace, "events": events,
                    }))
                }
                None => json_error(404, "no such session"),
            }
        }
        (Method::Post, "api/fork") => {
            let sid: u64 = body.get("s").and_then(|v| v.as_u64()).unwrap_or(0);
            let fid = state.lock().unwrap().fork(sid);
            match fid {
                Some(fid) => json_response(json!({ "fork": fid })),
                None => json_error(404, "no such session"),
            }
        }
        (Method::Post, "api/resume") => {
            let sid: u64 = body.get("s").and_then(|v| v.as_u64()).unwrap_or(0);
            let mut st = state.lock().unwrap();
            match st.session(sid) {
                Some(s) => {
                    s.push("note", "system", "Session resumed from last trajectory checkpoint.".to_string(), None);
                    s.finished = false;
                    json_response(json!({ "ok": true, "id": sid }))
                }
                None => json_error(404, "no such session"),
            }
        }
        // workspace
        (Method::Post, "api/workspace") => {
            let sid: u64 = body.get("s").and_then(|v| v.as_u64()).unwrap_or(0);
            let wp = body.get("path").and_then(|v| v.as_str()).unwrap_or(".");
            let mut st = state.lock().unwrap();
            match st.session(sid) {
                Some(s) => {
                    s.workspace = Some(wp.to_string());
                    s.push("note", "system", format!("Workspace set to {wp}"), None);
                    json_response(json!({ "ok": true, "workspace": wp }))
                }
                None => json_error(404, "no such session"),
            }
        }
        (Method::Get, "api/dir") => {
            let rel = q_param(query, "path").unwrap_or_default();
            let target = safe_join(root, &rel);
            let target = match target {
                Some(t) if t.starts_with(root.as_path()) => t,
                _ => return json_error(403, "path escapes workspace root"),
            };
            let mut entries: Vec<Value> = Vec::new();
            if let Ok(rd) = std::fs::read_dir(&target) {
                let mut items: Vec<_> = rd.filter_map(|e| e.ok()).collect();
                items.sort_by_key(|e| e.file_name());
                for e in items {
                    let ft = e.file_type().ok();
                    let is_dir = ft.map(|t| t.is_dir()).unwrap_or(false);
                    let size = e.metadata().ok().map(|m| m.len()).unwrap_or(0);
                    entries.push(json!({ "name": e.file_name().to_string_lossy(), "dir": is_dir, "size": size }));
                }
            } else {
                return json_error(404, "cannot read directory");
            }
            json_response(json!({ "path": rel, "entries": entries }))
        }
        (Method::Post, "api/dir") => {
            let rel = body.get("path").and_then(|v| v.as_str()).unwrap_or("");
            let name = body.get("name").and_then(|v| v.as_str()).unwrap_or("");
            if name.is_empty() || name.contains('/') {
                return json_error(400, "bad directory name");
            }
            let target = safe_join(root, rel);
            let target = match target {
                Some(t) if t.starts_with(root.as_path()) => t,
                _ => return json_error(403, "path escapes workspace root"),
            };
            let full = target.join(name);
            match std::fs::create_dir(&full) {
                Ok(()) => json_response(json!({ "ok": true, "path": format!("{rel}/{name}") })),
                Err(e) => json_error(500, &e.to_string()),
            }
        }
        (Method::Get, "api/file") => {
            let rel = q_param(query, "path").unwrap_or_default();
            let target = safe_join(root, &rel);
            let target = match target {
                Some(t) if t.starts_with(root.as_path()) && t.as_path() != root.as_path() => t,
                _ => return json_error(403, "path escapes workspace root"),
            };
            match std::fs::read(&target) {
                Ok(bytes) => {
                    if bytes.iter().take(4096).any(|&b| b == 0) {
                        return json_error(415, "binary file; text preview unavailable");
                    }
                    let cap = 100_000;
                    let truncated = bytes.len() > cap;
                    let text =
                        String::from_utf8_lossy(&bytes[..std::cmp::min(bytes.len(), cap)]).into_owned();
                    json_response(
                        json!({ "path": rel, "content": text, "truncated": truncated, "bytes": bytes.len() })
                    )
                }
                Err(e) => json_error(404, &e.to_string()),
            }
        }
        // chat / mock engine
        (Method::Post, "api/chat") => {
            let sid: u64 = body.get("s").and_then(|v| v.as_u64()).unwrap_or(0);
            let prompt = body.get("prompt").and_then(|v| v.as_str()).unwrap_or("").to_string();
            let model = body
                .get("model")
                .and_then(|v| v.as_str())
                .unwrap_or("deepseek-v3.2")
                .to_string();
            let mode = body
                .get("mode")
                .and_then(|v| v.as_str())
                .unwrap_or("Standard")
                .to_string();
            let permission = body
                .get("permission")
                .and_then(|v| v.as_str())
                .unwrap_or("Standard")
                .to_string();
            {
                let mut st = state.lock().unwrap();
                match st.session(sid) {
                    Some(s) => {
                        s.running = true;
                        s.finished = false;
                        s.push(
                            "note",
                            "system",
                            format!("User prompt: {prompt}  [model={model} mode={mode} permission={permission}]"),
                            None,
                        );
                    }
                    None => return json_error(404, "no such session"),
                }
            }
            mock::spawn_engine(Arc::clone(state), sid, prompt, model, mode, permission);
            json_response(json!({ "ok": true, "stream": format!("/api/stream?s={sid}") }))
        }
        (Method::Post, "api/approve") => {
            let sid: u64 = body.get("s").and_then(|v| v.as_u64()).unwrap_or(0);
            let ok = body.get("ok").and_then(|v| v.as_bool()).unwrap_or(true);
            let mut st = state.lock().unwrap();
            match st.session(sid) {
                Some(s) => {
                    s.approved.store(ok, Ordering::SeqCst);
                    json_response(json!({ "ok": true, "approved": ok, "awaiting": s.awaiting }))
                }
                None => json_error(404, "no such session"),
            }
        }
        // settings
        (Method::Get, "api/settings") => {
            let st = state.lock().unwrap();
            let mut models: Vec<String> = vec![
                "deepseek-v3.2".into(),
                "deepseek-r2".into(),
                "deepseek-v4.1-flash".into(),
            ];
            for p in &st.settings.providers {
                for m in &p.models {
                    if !models.contains(m) {
                        models.push(m.clone());
                    }
                }
            }
            json_response(json!({
                "default_model": st.settings.default_model,
                "providers": st.settings.providers,
                "models": models,
            }))
        }
        (Method::Post, "api/settings") => {
            let mut st = state.lock().unwrap();
            if let Some(m) = body.get("default_model").and_then(|v| v.as_str()) {
                st.settings.default_model = m.to_string();
            }
            if let Some(provs) = body.get("providers").and_then(|v| v.as_array()) {
                let list: Vec<Provider> = provs.iter().filter_map(|v| Provider::from_value(v.clone())).collect();
                st.settings.providers = list;
            }
            // immediate persistence, no restart
            std::fs::write(
                &st.settings_path,
                serde_json::to_string_pretty(&st.settings).unwrap_or_default(),
            )
            .ok();
            json_response(json!({ "ok": true, "providers": st.settings.providers }))
        }
        _ => json_error(404, "no such endpoint"),
    }
}

impl Provider {
    fn from_value(v: Value) -> Option<Self> {
        Some(Provider {
            name: v.get("name").and_then(|x| x.as_str()).unwrap_or("custom").to_string(),
            base_url: v.get("base_url").and_then(|x| x.as_str()).unwrap_or("").to_string(),
            api_key: v.get("api_key").and_then(|x| x.as_str()).unwrap_or("").to_string(),
            models: v
                .get("models")
                .and_then(|x| x.as_array())
                .map(|a| a.iter().filter_map(|m| m.as_str().map(String::from)).collect())
                .unwrap_or_default(),
        })
    }
}

