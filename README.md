# harness-webui-rust

A lightweight Rust single-binary Web UI inspired by DeepSeek Harness. It is a **UI and mock-agent implementation**, not a real LLM client.

## Run

```sh
./harness-webui-rust --port 3080
# open the launch URL printed by the server (it contains a one-time browser token)
```

The default bind is `127.0.0.1`. Non-loopback bind is refused unless `--insecure` is supplied. Static HTML/CSS/JS is embedded into the binary; no Node.js or frontend build is needed.

## Included UI

- Workbench sidebar, collapsible 236px / 56px rail, session list and new session.
- Workspace directory picker with breadcrumbs and one-level listing; create directory; composer stays disabled until a workspace is selected.
- Dark/light theme toggle.
- Model, reasoning level, Standard/Code/Minimal/Creator mode and Full access/Standard permission selectors.
- Scripted mock agent event streams over SSE: system prompt, folded reasoning, tool calls/results, sub-agent events, context injection and token usage.
- Standard permission mode pauses approval-gated tool calls behind Approve/Deny.
- Right docking tabs: workspace files, text preview and read-only Schedule catalog.
- Trajectory panel with source filtering, search, Resume and Fork.
- Settings → Models: custom OpenAI-compatible provider storage in `~/.dsh-webui-rust/settings.json`.

## Build

The board is deliberately not used for release compilation. `.github/workflows/build.yml` builds an `aarch64-unknown-linux-musl` release using GitHub Actions and uploads the static binary as an artifact.

## Limitations

The engine is mock/scripted and does not call DeepSeek or any other model. Session state is process-local; settings and the launch token persist. This is intended as a low-memory UI prototype.
