/* DeepSeek Harness style Web UI — vanilla JS frontend */
"use strict";

const $ = (id) => document.getElementById(id);

const state = {
  sessions: [],
  cur: null,          // current session object
  es: null,          // active EventSource
  settings: null,    // {models:[], providers:[]}
  wsDir: "",         // current directory in the picker / file tree
  pendingApprove: null,
};

/* ---------------- api ---------------- */
async function api(path, body) {
  const opt = body ? { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify(body) } : {};
  const r = await fetch(path, opt);
  if (!r.ok) {
    let msg = r.status;
    try { msg = (await r.json()).error || msg; } catch (e) {}
    throw new Error(String(msg));
  }
  return r.json();
}

/* ---------------- sessions ---------------- */
async function refreshSessions() {
  state.sessions = await api("/api/sessions");
  const ul = $("sessList");
  ul.innerHTML = "";
  for (const s of state.sessions) {
    const li = document.createElement("li");
    li.textContent = s.title;
    li.className = s.id === state.cur ? "active" : "";
    if (s.running) li.classList.add("running");
    li.onclick = () => openSession(s.id);
    ul.appendChild(li);
  }
}

async function openSession(id) {
  if (state.es) { state.es.close(); state.es = null; }
  const s = await api("/api/session?s=" + id);
  state.cur = s.id;
  renderEvents(s.events);
  if (s.workspace) {
    showChat(s.workspace);
  } else {
    showPicker();
  }
  refreshSessions();
}

async function newSession() {
  const r = await api("/api/sessions", { title: "会话 " + (state.sessions.length + 1) });
  await openSession(r.id);
}

/* ---------------- event rendering ---------------- */
function renderEvents(events) {
  const box = $("msgs");
  box.innerHTML = "";
  for (const e of events) box.appendChild(renderEvent(e));
  box.scrollTop = box.scrollHeight;
}

function renderEvent(e) {
  switch (e.t) {
    case "system": {
      const d = document.createElement("details");
      d.className = "evt system";
      d.innerHTML = "<summary>system prompt（默认折叠）</summary><pre></pre>";
      d.querySelector("pre").textContent = e.text;
      return d;
    }
    case "reasoning": {
      const d = document.createElement("details");
      d.className = "evt reasoning";
      d.innerHTML = "<summary>推理（默认折叠）</summary><pre></pre>";
      d.querySelector("pre").textContent = e.text;
      return d;
    }
    case "tool_call": {
      const div = document.createElement("div");
      div.className = "evt toolcall";
      const extra = e.extra || {};
      const label = extra.needs_approval ? "⚠ 需审批" : "▸";
      div.textContent = label + " " + e.text;
      if (extra.call_id) div.dataset.callId = extra.call_id;
      return div;
    }
    case "tool_result": {
      const div = document.createElement("div");
      div.className = "evt toolresult";
      const extra = e.extra || {};
      div.textContent = (extra.approved === false ? "✗ denied: " : "✓ result: ") + e.text;
      return div;
    }
    case "subagent": {
      const div = document.createElement("div");
      div.className = "evt subagent";
      div.textContent = "⑂ " + e.text;
      return div;
    }
    case "injection": {
      const div = document.createElement("div");
      div.className = "evt injection";
      div.textContent = "⤵ " + e.text;
      return div;
    }
    case "usage": {
      const div = document.createElement("div");
      div.className = "evt usage";
      const ex = e.extra || {};
      div.innerHTML = `<span>token 用量：</span>
        <span class="us">输入 ${ex.input ?? 0}（缓存 ${ex.cached ?? 0}）</span>
        <span class="us">输出 ${ex.output ?? 0}</span>`;
      return div;
    }
    case "note": {
      const div = document.createElement("div");
      div.className = "evt note";
      div.textContent = "· " + e.text;
      return div;
    }
    case "text":
    default: {
      const div = document.createElement("div");
      div.className = "evt answer";
      div.innerHTML = mdLite(e.text);
      return div;
    }
  }
}

/* minimal markdown: headers, bold, inline code, code fences, tables, lists */
function mdLite(src) {
  let s = esc(src).replace(/```([\s\S]*?)```/g, (m, c) => "<pre>" + c + "</pre>");
  s = s.replace(/^## (.*)$/gm, "<h3>$1</h3>");
  s = s.replace(/^# (.*)$/gm, "<h4>$1</h4>");
  s = s.replace(/\*\*(.+?)\*\*/g, "<b>$1</b>");
  s = s.replace(/`([^`]+)`/g, "<code>$1</code>");
  s = s.replace(/^- /gm, "<li>");
  s = s.replace(/\| (.+?) \|/g, (m, row) => "<tr>" + row.split("|").filter(Boolean).map((c) => "<td>" + c + "</td>").join("") + "</tr>");
  s = s.replace(/\n\n/g, "<br><br>");
  return s;
}
function esc(s) {
  return String(s).replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;");
}

/* ---------------- streaming ---------------- */
async function sendPrompt() {
  const prompt = $("prompt").value.trim();
  if (!prompt || state.cur == null) return;
  $("prompt").value = "";
  const opts = {
    s: state.cur,
    prompt,
    model: $("modelSel").value,
    reasoning: $("reasonSel").value,
    mode: $("modeSel").value,
    permission: $("permSel").value,
  };
  const r = await api("/api/chat", opts);
  startStream(r.stream, state.cur);
}

function startStream(url, sid) {
  if (state.es) state.es.close();
  const es = new EventSource(url);
  state.es = es;
  es.onmessage = (m) => {
    let e;
    try { e = JSON.parse(m.data); } catch (err) { return; }
    if (e.t === "end") return;
    const div = renderEvent(e);
    $("msgs").appendChild(div);
    $("msgs").scrollTop = $("msgs").scrollHeight;
    // approval-gated tool call
    const ex = e.extra || {};
    if (e.t === "tool_call" && ex.needs_approval) {
      state.pendingApprove = { sid, callId: ex.call_id, label: e.text };
      $("approvalText").textContent = e.text;
      $("approvalModal").classList.remove("hidden");
    }
  };
  es.addEventListener("end", () => {
    es.close();
    if (state.es === es) state.es = null;
    refreshSessions();
  });
  es.onerror = () => { /* keep the panel usable; events are also in /api/session */ };
}

function approve(ok) {
  if (state.pendingApprove) {
    api("/api/approve", { s: state.pendingApprove.sid, ok }).catch(() => {});
  }
  state.pendingApprove = null;
  $("approvalModal").classList.add("hidden");
}

/* ---------------- workspace picker ---------------- */
async function showPicker() {
  $("chatView").classList.add("hidden");
  $("workspacePicker").classList.remove("hidden");
  state.wsDir = "";
  await loadDir(state.wsDir, true);
}

function showChat(workspace) {
  $("workspacePicker").classList.add("hidden");
  $("chatView").classList.remove("hidden");
  $("wsChip").textContent = "工作区：" + workspace;
  $("wsHint").textContent = "";
  $("sendBtn").disabled = false;
  setWsPath(workspace);
  loadFileDir("");
}

async function loadDir(rel, isPicker) {
  const r = await api("/api/dir?path=" + encodeURIComponent(rel));
  const crumbs = $("crumbs");
  crumbs.innerHTML = "";
  const segs = rel ? rel.split("/") : [];
  let acc = "";
  crumbs.appendChild(crumb(isPicker ? "启动目录" : "…", "", true));
  for (const s of segs) {
    acc += (acc ? "/" : "") + s;
    crumbs.appendChild(crumb(s, acc, false));
  }
  const list = $("dirList");
  list.innerHTML = "";
  for (const e of r.entries) {
    const row = document.createElement("div");
    row.className = "dirrow" + (e.dir ? " dir" : "");
    row.textContent = (e.dir ? "📁 " : "📄 ") + e.name;
    row.onclick = async () => {
      if (e.dir) {
        state.wsDir = rel ? rel + "/" + e.name : e.name;
        await loadDir(state.wsDir, isPicker);
        if (!isPicker) loadFileDir(state.wsDir);
      } else if (!isPicker) {
        openFile(rel ? rel + "/" + e.name : e.name);
      }
    };
    list.appendChild(row);
  }
}
function crumb(label, rel, isRoot) {
  const c = document.createElement("span");
  c.className = "crumb" + (isRoot ? " root" : "");
  c.textContent = label;
  if (!isRoot) c.onclick = () => { state.wsDir = rel; loadDir(rel, true); };
  return c;
}

/* ---------------- right sidebar: files / preview / schedule ---------------- */
function setWsPath(p) { state.fileDir = p; }
async function loadFileDir(rel) {
  try {
    const r = await api("/api/dir?path=" + encodeURIComponent(rel));
    const list = $("fileList");
    list.innerHTML = "";
    const bc = $("fileCrumbs");
    bc.textContent = "📂 " + (rel || "(工作区根)");
    for (const e of r.entries) {
      const row = document.createElement("div");
      row.className = "dirrow" + (e.dir ? " dir" : "");
      row.textContent = (e.dir ? "📁 " : "📄 ") + e.name;
      row.onclick = () => {
        const p = rel ? rel + "/" + e.name : e.name;
        if (e.dir) loadFileDir(p);
        else openFile(p);
      };
      list.appendChild(row);
    }
  } catch (err) {
    $("fileList").textContent = "无法列出：" + err;
  }
}
async function openFile(p) {
  try {
    const r = await api("/api/file?path=" + encodeURIComponent(p));
    $("previewBox").textContent =
      (r.truncated ? "（截断显示，共 " + r.bytes + " 字节）\n" : "") + r.content;
    document.querySelector('.tab[data-tab="preview"]').click();
  } catch (err) {
    $("previewBox").textContent = "预览失败：" + err;
  }
}

/* ---------------- settings ---------------- */
async function loadSettings() {
  state.settings = await api("/api/settings");
  fillModels();
  renderProviders();
}
function fillModels() {
  const sel = $("modelSel");
  sel.innerHTML = "";
  for (const m of state.settings.models) {
    const o = document.createElement("option");
    o.value = m;
    o.textContent = m;
    if (m === state.settings.default_model) o.selected = true;
    sel.appendChild(o);
  }
  $("defModel").value = state.settings.default_model;
}
function renderProviders() {
  const box = $("provList");
  box.innerHTML = "";
  for (const p of state.settings.providers) {
    const div = document.createElement("div");
    div.className = "prov";
    div.textContent = p.name + " — " + p.base_url + "（" + p.models.join(", ") + "）";
    box.appendChild(div);
  }
}

/* ---------------- trajectory panel ---------------- */
function refreshTraj() {
  api("/api/session?s=" + state.cur).then((s) => {
    renderTraj(s.events);
  }).catch(() => {});
}
function renderTraj(events) {
  const src = $("trajSrc").value;
  const q = $("trajSearch").value.trim().toLowerCase();
  const body = $("trajBody");
  body.innerHTML = "";
  for (const e of events) {
    if (src && e.source !== src) continue;
    if (q && !(e.text.toLowerCase().includes(q) || e.t.toLowerCase().includes(q))) continue;
    const div = document.createElement("div");
    div.className = "trajrow src-" + e.source;
    div.innerHTML = `<span class="src">${e.source}</span> <span class="tt">${esc(e.t)}</span> <span>${esc(e.text).slice(0, 140)}</span>`;
    body.appendChild(div);
  }
}

/* ---------------- wiring ---------------- */
async function init() {
  const saved = localStorage.getItem("dsh-theme");
  if (saved) document.documentElement.dataset.theme = saved;
  $("themeBtn").onclick = () => {
    const h = document.documentElement;
    h.dataset.theme = h.dataset.theme === "dark" ? "light" : "dark";
    localStorage.setItem("dsh-theme", h.dataset.theme);
    $("darkCheck").checked = h.dataset.theme === "dark";
  };
  $("darkCheck").onchange = (ev) => {
    document.documentElement.dataset.theme = ev.target.checked ? "dark" : "light";
    localStorage.setItem("dsh-theme", document.documentElement.dataset.theme);
  };

  // sidebar collapse: full column <-> 56px rail
  $("collapseBtn").onclick = () => {
    document.body.classList.toggle("rail");
    $("collapseBtn").textContent = document.body.classList.contains("rail") ? "⇥" : "⇤";
  };

  // sessions
  $("newSess").onclick = newSession;

  // picker
  $("pickWsBtn").onclick = async () => {
    if (state.cur == null) { const r = await newSessionRaw(); }
    await api("/api/workspace", { s: state.cur, path: state.wsDir || "." });
    showChat(state.wsDir || ".");
  };
  $("mkDirBtn").onclick = async () => {
    const name = $("newDirName").value.trim();
    if (!name) return;
    await api("/api/dir", { path: state.wsDir, name });
    $("newDirName").value = "";
    await loadDir(state.wsDir, true);
  };

  // composer
  $("sendBtn").onclick = sendPrompt;
  $("prompt").addEventListener("keydown", (ev) => {
    if (ev.key === "Enter" && !ev.shiftKey) { ev.preventDefault(); sendPrompt(); }
  });

  // approval modal
  $("approveYes").onclick = () => approve(true);
  $("approveNo").onclick = () => approve(false);

  // right tabs
  document.querySelectorAll(".tab").forEach((t) => {
    t.onclick = () => {
      document.querySelectorAll(".tab").forEach((x) => x.classList.remove("active"));
      document.querySelectorAll(".tabpane").forEach((x) => x.classList.remove("active"));
      t.classList.add("active");
      $("tab" + cap(t.dataset.tab)).classList.add("active");
    };
  });

  // settings panel
  $("settingsBtn").onclick = () => {
    loadSettings();
    $("settingsPanel").classList.remove("hidden");
  };
  $("addProv").onclick = async () => {
    const p = {
      name: $("pName").value.trim() || "custom",
      base_url: $("pUrl").value.trim(),
      api_key: $("pKey").value.trim(),
      models: $("pModels").value.split(",").map((s) => s.trim()).filter(Boolean),
    };
    if (!p.name || !p.base_url) return;
    const cur = await api("/api/settings");
    const providers = (cur.providers || []).concat([p]);
    await api("/api/settings", { default_model: $("defModel").value, providers });
    loadSettings();
  };
  $("saveSettings").onclick = async () => {
    const cur = await api("/api/settings");
    await api("/api/settings", { default_model: $("defModel").value, providers: cur.providers });
    fillModels();
    $("settingsPanel").classList.add("hidden");
  };

  // generic close buttons
  document.querySelectorAll("[data-close]").forEach((b) => {
    b.onclick = () => $(b.dataset.close).classList.add("hidden");
  });

  // trajectory
  $("trajBtn").onclick = async () => {
    refreshTraj();
    $("trajPanel").classList.toggle("hidden");
  };
  $("trajSrc").onchange = refreshTraj;
  $("trajSearch").oninput = refreshTraj;
  $("resumeBtn").onclick = async () => {
    await api("/api/resume", { s: state.cur });
    refreshTraj();
  };
  $("forkBtn").onclick = async () => {
    const r = await api("/api/fork", { s: state.cur });
    $("trajPanel").classList.add("hidden");
    openSession(r.fork);
  };

  // schedule (read-only catalog)
  const sched = [
    { when: "09:00 daily", what: "拉取 GitHub Actions 构建状态" },
    { when: "18:00 daily", what: "内存/swap 使用巡检" },
    { when: "weekly", what: "公众号选题雷同度扫描" },
  ];
  const sl = $("schedList");
  for (const s of sched) {
    const li = document.createElement("li");
    li.innerHTML = `<span class="when">${s.when}</span> ${esc(s.what)} <span class="off">disabled</span>`;
    sl.appendChild(li);
  }

  // boot
  await loadSettings();
  await refreshSessions();
  if (state.sessions.length === 0) await newSession();
  else await openSession(state.sessions[state.sessions.length - 1].id);
}

async function newSessionRaw() {
  const r = await api("/api/sessions", { title: "新会话" });
  state.cur = r.id;
  renderEvents([]);
}

function cap(s) { return s.charAt(0).toUpperCase() + s.slice(1); }

document.addEventListener("DOMContentLoaded", init);
