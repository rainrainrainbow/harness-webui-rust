use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use serde_json::{json, Value};

use crate::state::AppState;

/// One scripted step of the mock agent loop.
struct Step {
    t: &'static str,
    source: &'static str,
    text: &'static str,
    delay_ms: u64,
    needs_approval: bool,
    call_id: &'static str,
    result: &'static str,
    extra: Option<Value>,
}

fn step(
    t: &'static str,
    source: &'static str,
    text: &'static str,
    delay_ms: u64,
) -> Step {
    Step {
        t,
        source,
        text,
        delay_ms,
        needs_approval: false,
        call_id: "",
        result: "",
        extra: None,
    }
}

fn tool_step(
    label: &'static str,
    delay_ms: u64,
    needs_approval: bool,
    call_id: &'static str,
    result: &'static str,
) -> Step {
    Step {
        t: "tool_call",
        source: "tool",
        text: label,
        delay_ms,
        needs_approval,
        call_id,
        result,
        extra: None,
    }
}

fn script_repo() -> Vec<Step> {
    vec![
        step(
            "system",
            "system",
            "You are DeepSeek Harness, an open-source agent built on the Cordis \
             everything-is-a-plugin kernel. Workspace is mounted at /work. \
             Standard toolset loaded: file ops, shell, web search, planning, \
             subagents.",
            200,
        ),
        step(
            "reasoning",
            "model",
            "Request: summarize this repository. Plan: list top-level files → read \
             Cargo.toml → catalog source modules → write the summary. I will start \
             with a directory scan.",
            500,
        ),
        tool_step("ls . (directory listing)", 600, false, "call_ls", "24 entries: src/, web/, \
          .github/workflows/build.yml, Cargo.toml, Cargo.lock, README.md …"),
        step(
            "injection",
            "context",
            "Context injected: Cargo.toml dependencies resolved into the active window \
             (tiny_http, serde, serde_json — 3 direct dependencies).",
            400,
        ),
        step(
            "subagent",
            "subagent",
            "Sub-agent 'scanner' scheduled: catalog source modules and public API.",
            500,
        ),
        step(
            "subagent",
            "subagent",
            "Sub-agent 'scanner' finished: 1 crate, 4 modules (server, state, mock, events), \
             public surface = HTTP API + embedded Web UI.",
            800,
        ),
        step("reasoning", "model", "All evidence gathered. Drafting the summary now.", 300),
        Step {
            t: "text",
            source: "model",
            text: "## Repository summary\n\n- **Purpose**: single-binary Web UI server \
              (DeepSeek Harness style workbench).\n- **Stack**: Rust + tiny_http; embedded \
              static frontend (HTML/JS/CSS) via `include_str!`.\n- **Modules**: `server` \
              (HTTP routing), `state` (sessions/events/settings), `mock` (scripted agent \
              loop), `events` (SSE stream).\n- **Notable**: token-gated launch URL, \
              per-session append-only trajectory, approval-gated tool calls under the \
              Standard permission preset.",
            delay_ms: 700,
            needs_approval: false,
            call_id: "",
            result: "",
            extra: None,
        },
        Step {
            t: "usage",
            source: "model",
            text: "Tokens this turn",
            delay_ms: 100,
            needs_approval: false,
            call_id: "",
            result: "",
            extra: Some(json!({ "input": 412, "output": 263, "cached": 96 })),
        },
    ]
}

fn script_bug() -> Vec<Step> {
    vec![
        step(
            "system",
            "system",
            "You are DeepSeek Harness. Workspace mounted. Shell + editor + test runner \
             available. Permission preset active: Standard (tool calls require approval).",
            200,
        ),
        step(
            "reasoning",
            "model",
            "Repro first: run the test suite to confirm the failure, then read the \
             failing parser, patch, re-run. Step 1: cargo test --lib.",
            500,
        ),
        tool_step(
            "cargo test --lib (run in workspace)",
            800,
            true,
            "call_test1",
            "3 tests: 2 passed, 1 FAILED — test_parser::nested_brackets \
             (assertion failed: depth == 3)",
        ),
        tool_step(
            "read src/parser.rs (lines 40–80)",
            600,
            true,
            "call_read",
            "Found off-by-one: `depth` decremented before the matching-close check, \
             so nested brackets one level deep report depth-1.",
        ),
        tool_step("edit src/parser.rs (+2 −1)", 500, true, "call_edit", "Patch applied: \
          move the `depth -= 1` after the close-token comparison."),
        tool_step("cargo test --lib (re-run)", 800, false, "call_test2", "3 tests passed \
          in 0.41s. No regressions."),
        Step {
            t: "text",
            source: "model",
            text: "Fixed. The parser decremented the bracket depth *before* comparing the \
              close token, which miscounted nested brackets by one. Reordered the two \
              statements in `src/parser.rs`; the suite is green (3/3).\n\nFiles changed: \
              `src/parser.rs` (+2 −1).",
            delay_ms: 600,
            needs_approval: false,
            call_id: "",
            result: "",
            extra: None,
        },
        Step {
            t: "usage",
            source: "model",
            text: "Tokens this turn",
            delay_ms: 100,
            needs_approval: false,
            call_id: "",
            result: "",
            extra: Some(json!({ "input": 903, "output": 418, "cached": 211 })),
        },
    ]
}

fn script_doc() -> Vec<Step> {
    vec![
        step(
            "system",
            "system",
            "You are DeepSeek Harness. Workspace mounted. Document toolset loaded \
             (markdown writer, table analysis).",
            200,
        ),
        step(
            "reasoning",
            "model",
            "Deliverable: a short ops report. Gather: top memory consumers, swap usage, \
             load average — then render a markdown report with tables and save it.",
            500,
        ),
        tool_step(
            "create docs/OPS-REPORT.md",
            700,
            true,
            "call_doc",
            "Wrote docs/OPS-REPORT.md (1.2 KB, 18 lines).",
        ),
        step(
            "injection",
            "context",
            "Context injected: `free -m` snapshot (total 784 / available ~127 / swap 4.4 GB) \
             attached as table rows for the report.",
            400,
        ),
        Step {
            t: "text",
            source: "model",
            text: "Report saved to `docs/OPS-REPORT.md` and rendered below — iterate on it \
              by telling me what to change.",
              delay_ms: 600,
            needs_approval: false,
            call_id: "",
            result: "",
            extra: Some(
                json!({ "artifact": { "name": "OPS-REPORT.md", "type": "document",
                    "content": "# Ops report (auto-generated)\n\n| metric | value |\n|---|---|\n| RAM total | 784 MiB |\n| RAM available | ~127 MiB |\n| swap in use | 4.4 GB configured (zram + 2×2 GB files) |\n| load (1/5/15) | 0.4 / 0.5 / 0.5 |\n| top consumer | hermes (agent gateway) |\n\n## notes\n- memory is chronically tight; heavy builds must go to CI.\n- no anomalies detected in this window.\n" } }),
            ),
        },
        Step {
            t: "usage",
            source: "model",
            text: "Tokens this turn",
            delay_ms: 100,
            needs_approval: false,
            call_id: "",
            result: "",
            extra: Some(json!({ "input": 344, "output": 187, "cached": 52 })),
        },
    ]
}

fn script_generic() -> Vec<Step> {
    vec![
        step(
            "system",
            "system",
            "You are DeepSeek Harness (mock mode). Working in the selected workspace.",
            200,
        ),
        step(
            "reasoning",
            "model",
            "Task received. I'll break it into steps, check the workspace, then report \
             findings.",
            400,
        ),
        tool_step("ls . (workspace scan)", 500, false, "call_ls2", "8 entries: src/, web/, \
          docs/, Cargo.toml, README.md …"),
        step(
            "subagent",
            "subagent",
            "Sub-agent 'reviewer' scheduled: cross-check the task against workspace files.",
            500,
        ),
        step(
            "subagent",
            "subagent",
            "Sub-agent 'reviewer' finished: no blockers; suggested doing a quick \
             read-only pass before edits.",
            700,
        ),
        Step {
            t: "text",
            source: "model",
            text: "Done with the first pass. Summary: workspace scanned (8 entries), \
              sub-agent review found no blockers. Next steps are listed in the \
              trajectory log. Tell me which one to execute.",
            delay_ms: 500,
            needs_approval: false,
            call_id: "",
            result: "",
            extra: None,
        },
        Step {
            t: "usage",
            source: "model",
            text: "Tokens this turn",
            delay_ms: 100,
            needs_approval: false,
            call_id: "",
            result: "",
            extra: Some(json!({ "input": 210, "output": 96, "cached": 34 })),
        },
    ]
}

fn pick(prompt: &str) -> Vec<Step> {
    let p = prompt.to_lowercase();
    if p.contains("修") || p.contains("bug") || p.contains("fix") || p.contains("测试") {
        script_bug()
    } else if p.contains("文档") || p.contains("报告") || p.contains("report") || p.contains("汇报") {
        script_doc()
    } else if p.contains("总结") || p.contains("summar") || p.contains("仓库") || p.contains("repo") {
        script_repo()
    } else {
        script_generic()
    }
}

/// Run the scripted agent loop for a session, appending events under the global lock.
/// Blocks at approval-gated tool calls when `permission == "Standard"`.
pub fn spawn_engine(
    state: Arc<Mutex<AppState>>,
    sid: u64,
    prompt: String,
    model: String,
    mode: String,
    permission: String,
) {
    let script = pick(&prompt);
    let gate = permission == "Standard";

    thread::spawn(move || {
        for (i, s) in script.iter().enumerate() {
            if s.t == "tool_call" {
                {
                    let mut st = state.lock().unwrap();
                    if let Some(sess) = st.session(sid) {
                        sess.push("tool_call", "tool", s.text.to_string(), Some(json!({
                            "call_id": s.call_id,
                            "needs_approval": gate && s.needs_approval,
                        })));
                    }
                }
                thread::sleep(Duration::from_millis(s.delay_ms));
                if gate && s.needs_approval {
                    // tell /api/approve which call we are blocked on
                    {
                        let mut st = state.lock().unwrap();
                        if let Some(sess) = st.session(sid) {
                            sess.awaiting = Some(s.call_id.to_string());
                        }
                    }
                    // wait for the UI to approve/deny
                    let mut decided = false;
                    let mut ok = false;
                    let deadline = std::time::Instant::now() + Duration::from_secs(300);
                    loop {
                        let (appr, await_flag) = {
                            let st = state.lock().unwrap();
                            match st.sessions.iter().find(|x| x.id == sid) {
                                Some(sess) => (
                                    sess.approved.load(Ordering::SeqCst),
                                    sess.awaiting.as_deref() == Some(s.call_id),
                                ),
                                None => break,
                            }
                        };
                        if await_flag && appr {
                            ok = true;
                            decided = true;
                            break;
                        }
                        if std::time::Instant::now() > deadline {
                            break;
                        }
                        thread::sleep(Duration::from_millis(120));
                    }
                    eprintln!(
                        "engine[{}] gate[{}] passed: decided={} ok={}",
                        sid, s.call_id, decided, ok
                    );
                    // clear the flag for the next gated call
                    let mut st = state.lock().unwrap();
                    if let Some(sess) = st.session(sid) {
                        sess.approved.store(false, Ordering::SeqCst);
                        sess.awaiting = None;
                    }
                    let result_text = if decided {
                        if ok {
                            s.result
                        } else {
                            "User DENIED this tool call. Stopping and asking how to proceed."
                        }
                    } else {
                        "Approval timed out; skipped (mock)."
                    };
                    let mut st = state.lock().unwrap();
                    if let Some(sess) = st.session(sid) {
                        sess.push(
                            "tool_result",
                            "tool",
                            result_text.to_string(),
                            Some(json!({ "call_id": s.call_id, "approved": decided && ok })),
                        );
                    }
                    if decided && !ok {
                        // denied → stop the script early with a note
                        let mut st = state.lock().unwrap();
                        if let Some(sess) = st.session(sid) {
                            sess.push(
                                "note",
                                "system",
                                "Execution paused after a denied tool call. Resume when ready."
                                    .to_string(),
                                None,
                            );
                        }
                        let mut st = state.lock().unwrap();
                        if let Some(sess) = st.session(sid) {
                            sess.running = false;
                            sess.finished = true;
                        }
                        return;
                    }
                } else {
                    // ungated: emit the result straight away
                    let mut st = state.lock().unwrap();
                    if let Some(sess) = st.session(sid) {
                        sess.push("tool_result", "tool", s.result.to_string(), Some(json!({ "call_id": s.call_id })));
                    }
                }
            } else if s.t == "usage" {
                // emit all remaining steps as-is (text/subagent/injection/etc.)
                let mut st = state.lock().unwrap();
                if let Some(sess) = st.session(sid) {
                    sess.push(s.t, s.source, s.text.to_string(), s.extra.clone());
                }
                thread::sleep(Duration::from_millis(150));
            } else {
                let mut st = state.lock().unwrap();
                if let Some(sess) = st.session(sid) {
                    sess.push(s.t, s.source, s.text.to_string(), s.extra.clone());
                }
                thread::sleep(Duration::from_millis(s.delay_ms));
            }
            let _ = i;
        }
        {
            let mut st = state.lock().unwrap();
            if let Some(sess) = st.session(sid) {
                let _ = (model.as_str(), mode.as_str());
                sess.running = false;
                sess.finished = true;
            }
        }
        // unused model/mode labels are part of the trajectory metadata; log for debugging
        eprintln!(
            "mock engine done for session {} (model={} mode={})",
            sid, model, mode
        );
    });
}

/// Mark the session as awaiting approval for a given call id (called from the HTTP layer
/// when a gated tool_call event was just appended, so /api/approve knows where to point).
pub fn mark_awaiting(state: &Arc<Mutex<AppState>>, sid: u64, call_id: &str) {
    let mut st = state.lock().unwrap();
    if let Some(sess) = st.session(sid) {
        sess.awaiting = Some(call_id.to_string());
    }
}
