mod mock;
mod state;

use std::io::{Read as IoRead, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{json, Value};
use tiny_http::{Data, Header, Method, Request, Response, Server};
use tiny_http::stream::Stream;

use state::{AppState, Event, Provider};

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
    let dir = PathBuf::from(home).join(".dsh-webui-rust");
    std::fs::create_dir_all(&dir).ok();
    let state_path = dir.join("state.json");
    let settings_path = dir.join("settings.json");

    let (token, fresh_token) = match std::fs::read_to_string(&state_path) {
        Ok(s) => match serde_json::from_str::<Value>(&s).ok() {
            Some(v) => v.get("token").and_then(|t| t.as_str()).map(|t| (t.to_string(), false)).unwrap_or_else(|| {
                (rand_token(), true)
            }),
            None => (rand_token(), true),
        },
        Err(_) => (rand_token(), true),
    };
    if fresh_token {
        let _ = std::fs::write(
            &state_path,
            serde_json::to_string_pretty(&json!({ "token": token })) .unwrap_or_default(),
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
    let root = root.canonicalize().unwrap_or(root);
    let root = Arc::new(root);

    let addr = format!("{bind}:{port}");
    let server = Server::http(&addr).unwrap_or_else(|e| {
        eprintln!("bind {addr} failed: {e}");
        std::process::exit(1);
    });
    let launch_url = format!(
        "http://{addr}/?token={token}",
        addr = if bind == "127.0.0.1" { "127.0.0.1" } else { bind }
    );
    println!("DeepSeek Harness Web UI (mock) listening on {addr}");
    println!("open:  {launch_url}");
    println!("workspace root: {}", root.display());
    if !insecure {
        println!("remote access refused (loopback only). Re-run with --bind 0.0.0.0 --insecure to expose.");
    }

    for request in server.incoming_requests() {
        handle(request, &state, &root);
    }
}

fn rand_token() -> String {
    let mut out = String::with_capacity(32);
    for b in std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos()
        .to_string()
        .bytes()
        .chain(std::process::id().to_string().bytes())
    {
        out.push((b'0' + b % 9) as char);
    }
    for _ in 0..40 {
        out.push('x');
    }
    // xor-ish scramble so two launches differ
    let seed = std::time::SystemTime::now().subsec_nanos();
    for (i, c) in out.bytes().enumerate() {
        out.replace_range(
            i..i + 1,
            &((c + (seed % 7) as u8).to_string()),
        );
    }
    out
}

fn handle(request: Request, state: &Arc<Mutex<AppState>>, root: &Arc<PathBuf>) {
    let method = request.method().clone();
    let url = request.url().to_string();
    let (path, query) = match url.find('?') {
        Some(n) => (&url[..n], &url[n + 1..]),
        None => (url.as_str(), ""),
    };
    let path = path.trim_start_matches('/');
    let mut body = String::new();
    request.as_reader().read_to_string(&mut body).ok();
    let body: Value = if body.is_empty() {
        Value::Null
    } else {
        serde_json::from_str(&body).unwrap_or(Value::Null)
    };

    let response = route(method, path, query, &body, state, root, request.headers());
    let _ = request.respond(response);
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
            let hi = (bytes[i + 1] as char).to_string();
            let lo = (bytes[i + 2] as char).to_string();
            if let Ok(v) = u8::from_str_radix(&hi + &lo, 16) {
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

fn json_response(v: Value) -> Response {
    let b = serde_json::to_string(&v).unwrap_or_else(|_| "{}".into());
    Response::from_string(b)
        .with_header(Header::from_bytes(&b"Content-Type", &b"application/json").unwrap())
}

fn json_error(status: &str, msg: &str) -> Response {
    let r = Response::from_string(serde_json::to_string(&json!({"error": msg})).unwrap())
        .with_status_code(status)
        .with_header(Header::from_bytes(&b"Content-Type", &b"application/json").unwrap());
    r
}

fn authed(request_headers: &[Header], query: &str, state: &Arc<Mutex<AppState>>) -> bool {
    let token = {
        let st = state.lock().unwrap();
        st.token.clone()
    };
    let cookie_ok = request_headers
        .iter()
        .any(|h| {
            h.field() == "Cookie"
                && h.value.to_string().ok().unwrap_or_default().contains(&format!("dsh={token}"))
        });
    if cookie_ok {
        return true;
    }
    q_param(query, "token").as_deref() == Some(token.as_str())
}

fn safe_join(root: &Path, rel: &str) -> Option<PathBuf> {
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
    Some(root.join(stack.iter().collect::<Vec<_>>().join("/")))
}

fn route(
    method: &Method,
    path: &str,
    query: &str,
    body: &Value,
    state: &Arc<Mutex<AppState>>,
    root: &Arc<PathBuf>,
    req_headers: &[Header],
) -> Response {
    // ---- static / auth ------------------------------------------------------
    if path.is_empty() {
        let token = {
            let st = state.lock().unwrap();
            st.token.clone()
        };
        if let Some(t) = q_param(query, "token") {
            if t == token {
                let html = include_str!("../web/index.html");
                let mut r = Response::from_string(html.to_string());
                r = r.with_header(
                    Header::from_bytes(
                        &b"Content-Type",
                        &b"text/html; charset=utf-8",
                    )
                    .unwrap(),
                );
                r = r.with_header(
                    Header::from_bytes(
                        &b"Set-Cookie",
                        format!("dsh={token}; Path=/; HttpOnly").as_bytes(),
                    )
                    .unwrap(),
                );
                return r;
            }
        }
        if authed(req_headers, query, state) {
            let html = include_str!("../web/index.html");
            return Response::from_string(html.to_string()).with_header(
                Header::from_bytes(&b"Content-Type", &b"text/html; charset=utf-8").unwrap(),
            );
        }
        return json_error("401", "missing or bad token; open the launch URL printed at server start");
    }
    if path == "app.js" {
        return Response::from_string(include_str!("../web/app.js").to_string()).with_header(
            Header::from_bytes(&b"Content-Type", &b"application/javascript; charset=utf-8").unwrap(),
        );
    }
    if path == "style.css" {
        return Response::from_string(include_str!("../web/style.css").to_string()).with_header(
            Header::from_bytes(&b"Content-Type", &b"text/css; charset=utf-8").unwrap(),
        );
    }
    if !path.starts_with("api/") {
        return json_error("404", "not found");
    }
    if !authed(req_headers, query, state) {
        return json_error("401", "unauthenticated");
    }

    // ---- api ----------------------------------------------------------------
    match (method, path) {
        // sessions
        (&Method::Get, "api/sessions") => {
            let st = state.lock().unwrap();
            let list: Vec<_> = st.sessions.iter().map(|s| s.view()).collect();
            json_response(Value::Array(list.into_iter().map(Value::from).collect()))
        }
        (&Method::Post, "api/sessions") => {
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
        (&Method::Get, "api/session") => {
            let sid: u64 = q_param(query, "s").and_then(|v| v.parse().ok()).unwrap_or(0);
            let st = state.lock().unwrap();
            match st.sessions.iter().find(|s| s.id == sid) {
                Some(s) => {
                    let events: Vec<Value> = s.events.iter().map(Value::from).collect();
                    json_response(json!({
                        "id": s.id, "title": s.title, "running": s.running,
                        "finished": s.finished, "workspace": s.workspace, "events": events,
                    }))
                }
                None => json_error("404", "no such session"),
            }
        }
        (&Method::Post, "api/fork") => {
            let sid: u64 = body.get("s").and_then(|v| v.as_u64()).unwrap_or(0);
            let fid = state.lock().unwrap().fork(sid);
            match fid {
                Some(fid) => json_response(json!({ "fork": fid })),
                None => json_error("404", "no such session"),
            }
        }
        (&Method::Post, "api/resume") => {
            let sid: u64 = body.get("s").and_then(|v| v.as_u64()).unwrap_or(0);
            let mut st = state.lock().unwrap();
            match st.session(sid) {
                Some(s) => {
                    s.push("note", "system", "Session resumed from last trajectory checkpoint.".to_string(), None);
                    s.finished = false;
                    json_response(json!({ "ok": true, "id": sid }))
                }
                None => json_error("404", "no such session"),
            }
        }
        // workspace
        (&Method::Post, "api/workspace") => {
            let sid: u64 = body.get("s").and_then(|v| v.as_u64()).unwrap_or(0);
            let wp = body.get("path").and_then(|v| v.as_str()).unwrap_or(".");
            let mut st = state.lock().unwrap();
            match st.session(sid) {
                Some(s) => {
                    s.workspace = Some(wp.to_string());
                    s.push("note", "system", format!("Workspace set to {wp}"), None);
                    json_response(json!({ "ok": true, "workspace": wp }))
                }
                None => json_error("404", "no such session"),
            }
        }
        (&Method::Get, "api/dir") => {
            let rel = q_param(query, "path").unwrap_or_default();
            let target = safe_join(root, &rel);
            let target = match target {
                Some(t) if t.starts_with(root.as_path()) => t,
                _ => return json_error("403", "path escapes workspace root"),
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
                return json_error("404", "cannot read directory");
            }
            json_response(json!({ "path": rel, "entries": entries }))
        }
        (&Method::Post, "api/dir") => {
            let rel = body.get("path").and_then(|v| v.as_str()).unwrap_or("");
            let name = body.get("name").and_then(|v| v.as_str()).unwrap_or("");
            if name.is_empty() || name.contains('/') {
                return json_error("400", "bad directory name");
            }
            let target = safe_join(root, rel);
            let target = match target {
                Some(t) if t.starts_with(root.as_path()) => t,
                _ => return json_error("403", "path escapes workspace root"),
            };
            let full = target.join(name);
            match std::fs::create_dir(&full) {
                Ok(()) => json_response(json!({ "ok": true, "path": format!("{rel}/{name}") })),
                Err(e) => json_error("500", &e.to_string()),
            }
        }
        (&Method::Get, "api/file") => {
            let rel = q_param(query, "path").unwrap_or_default();
            let target = safe_join(root, &rel);
            let target = match target {
                Some(t) if t.starts_with(root.as_path()) && t != *root => t,
                _ => return json_error("403", "path escapes workspace root"),
            };
            match std::fs::read(&target) {
                Ok(bytes) => {
                    if bytes.iter().take(4096).any(|&b| b == 0) {
                        return json_error("415", "binary file; text preview unavailable");
                    }
                    let cap = 100_000;
                    let truncated = bytes.len() > cap;
                    let text = String::from_utf8_lossy(&bytes[..std::cmp::min(bytes.len(), cap)]).into_owned();
                    json_response(json!({ "path": rel, "content": text, "truncated": truncated, "bytes": bytes.len() }))
                }
                Err(e) => json_error("404", &e.to_string()),
            }
        }
        // chat / mock engine
        (&Method::Post, "api/chat") => {
            let sid: u64 = body.get("s").and_then(|v| v.as_u64()).unwrap_or(0);
            let prompt = body
                .get("prompt")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
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
                    None => return json_error("404", "no such session"),
                }
            }
            mock::spawn_engine(
                Arc::clone(state),
                sid,
                prompt,
                model,
                mode,
                permission,
            );
            json_response(json!({ "ok": true, "stream": format!("/api/stream?s={sid}") }))
        }
        (&Method::Get, "api/stream") => {
            let sid: u64 = q_param(query, "s").and_then(|v| v.parse().ok()).unwrap_or(0);
            let offset = {
                let st = state.lock().unwrap();
                match st.sessions.iter().find(|s| s.id == sid) {
                    Some(s) => s.events.len(),
                    None => 0,
                }
            };
            let st = Arc::clone(state);
            let stream = SseStream {
                sid,
                start_offset: offset,
                state: st,
            };
            let mut r = Response::from_data(Data::from_stream(Box::new(stream)));
            r = r.with_header(
                Header::from_bytes(&b"Content-Type", &b"text/event-stream").unwrap(),
            );
            r = r
                .with_header(Header::from_bytes(&b"Cache-Control", &b"no-cache").unwrap())
                .with_header(Header::from_bytes(&b"X-Accel-Buffering", &b"no").unwrap());
            r
        }
        (&Method::Post, "api/approve") => {
            let sid: u64 = body.get("s").and_then(|v| v.as_u64()).unwrap_or(0);
            let ok = body.get("ok").and_then(|v| v.as_bool()).unwrap_or(true);
            let mut st = state.lock().unwrap();
            match st.session(sid) {
                Some(s) => {
                    s.approved.store(ok, Ordering::SeqCst);
                    json_response(json!({ "ok": true, "approved": ok, "awaiting": s.awaiting }))
                }
                None => json_error("404", "no such session"),
            }
        }
        // settings
        (&Method::Get, "api/settings") => {
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
        (&Method::Post, "api/settings") => {
            let mut st = state.lock().unwrap();
            if let Some(m) = body.get("default_model").and_then(|v| v.as_str()) {
                st.settings.default_model = m.to_string();
            }
            if let Some(provs) = body.get("providers").and_then(|v| v.as_array()) {
                let mut list: Vec<Provider> = provs
                    .iter()
                    .filter_map(|v| serde_json::from_value(Provider::from(v.clone())).ok())
                    .collect();
                if st.settings.providers.len() < list.len() {
                    list.truncate(list.len());
                }
                st.settings.providers = list;
            }
            // immediate persistence, no restart
            std::fs::write(&st.settings_path, serde_json::to_string_pretty(&st.settings).unwrap()).ok();
            json_response(json!({ "ok": true, "providers": st.settings.providers }))
        }
        _ => json_error("404", "no such endpoint"),
    }
}

impl From<Value> for Provider {
    fn from(v: Value) -> Self {
        Provider {
            name: v.get("name").and_then(|x| x.as_str()).unwrap_or("custom").to_string(),
            base_url: v
                .get("base_url")
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .to_string(),
            api_key: v
                .get("api_key")
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .to_string(),
            models: v
                .get("models")
                .and_then(|x| x.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|m| m.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default(),
        }
    }
}

/// SSE stream: re-emit events appended after `start_offset`; close when the session
/// stops running and we have caught up.
struct SseStream {
    sid: u64,
    start_offset: usize,
    state: Arc<Mutex<AppState>>,
}

impl Stream for SseStream {
    fn write_to<W: Write + Send>(&self, w: &mut W) -> std::io::Result<()> {
        let mut offset = self.start_offset;
        loop {
            let (new_events, running, gone) = {
                let st = self.state.lock().unwrap();
                let s = match st.sessions.iter().find(|x| x.id == self.sid) {
                    Some(s) => s,
                    None => (Vec::new(), false, true),
                };
                let new: Vec<Event> = s.events.get(offset..).unwrap_or(&[]).to_vec();
                (new, s.running, s.events.len() == offset)
            };
            let _ = gone;
            for e in &new_events {
                let payload = serde_json::to_string(e).unwrap_or_default();
                w.write_all(format!("data: {payload}\n\n").as_bytes())?;
            }
            if !new_events.is_empty() {
                w.flush()?;
                offset += new_events.len();
                continue;
            }
            if !running {
                w.write_all(b"event: end\ndata: {}\n\n")?;
                w.flush()?;
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(150));
        }
    }
}
