"use strict";

/* ============================================================
 * android-AI-Novel-Agent · Web UI
 * 参考原桌面项目的交互与信息架构（书架 / 开书 / 工作台 / 运行记录 / 模型设置）
 * ============================================================ */

const $ = (s, r = document) => r.querySelector(s);
const $$ = (s, r = document) => Array.from(r.querySelectorAll(s));

const state = {
  settings: null,
  novelId: null,
  novel: null,
  es: null,
  stream: { chapter: null, text: "" },
  pollTimer: null,
  shelfItems: [],
  shelfFilter: "all",
};

/* ---------------- 工具 ---------------- */

function esc(v) {
  return String(v ?? "").replace(/[&<>"']/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" }[c]));
}

async function api(path, opts) {
  const r = await fetch(path, opts);
  if (!r.ok) throw new Error((await r.text()) || r.statusText);
  return r.json();
}

function toast(msg, isError = false) {
  const el = document.createElement("div");
  el.className = "toast" + (isError ? " error" : "");
  el.textContent = msg;
  $("#toasts").appendChild(el);
  setTimeout(() => {
    el.classList.add("hide");
    setTimeout(() => el.remove(), 200);
  }, 3600);
}

function fmtTime(iso) {
  if (!iso) return "—";
  const d = new Date(iso);
  if (Number.isNaN(d.getTime())) return iso;
  const p = (n) => String(n).padStart(2, "0");
  return `${p(d.getMonth() + 1)}-${p(d.getDate())} ${p(d.getHours())}:${p(d.getMinutes())}`;
}

const NOVEL_STATUS = {
  created: { label: "已创建", cls: "chip" },
  outlined: { label: "已规划", cls: "chip-info" },
  writing: { label: "创作中", cls: "chip-info" },
  drafted: { label: "已完成", cls: "chip-success" },
  failed: { label: "异常", cls: "chip-danger" },
};
const RUN_STATUS = { running: "执行中", done: "已完成", failed: "失败" };
const STAGE_LABEL = { brief: "整理立项", outline: "章节规划", draft: "正文生成", review: "章节审校", done: "全部完成", failed: "已中断" };

function statusChip(s) {
  const m = NOVEL_STATUS[s] || { label: s || "—", cls: "chip" };
  return `<span class="chip ${m.cls}">${esc(m.label)}</span>`;
}

function parseReview(raw) {
  if (!raw) return null;
  try {
    const j = JSON.parse(raw);
    return { ok: !!j.ok, issues: Array.isArray(j.issues) ? j.issues : [] };
  } catch {
    return null;
  }
}

function reviewChip(raw) {
  const r = parseReview(raw);
  if (!r) return `<span class="chip">—</span>`;
  if (r.ok && r.issues.length === 0) return `<span class="chip chip-success">通过</span>`;
  if (r.ok) return `<span class="chip chip-warning">提醒 ${r.issues.length}</span>`;
  return `<span class="chip chip-danger">需处理 ${r.issues.length}</span>`;
}

/* ---------------- 路由 ---------------- */

const PAGES = {
  "#/shelf": { title: "书架", subtitle: "管理你的作品与创作进度", view: "#view-shelf" },
  "#/create": { title: "新建作品", subtitle: "从一个想法开始，AI 逐步写出整本小说", view: "#view-create" },
  "#/runs": { title: "运行记录", subtitle: "创作任务的执行状态（只读，操作请回到作品工作台）", view: "#view-runs" },
  "#/settings": { title: "模型设置", subtitle: "配置用于创作的模型服务", view: "#view-settings" },
};

function parseRoute() {
  const hash = location.hash || "#/shelf";
  const m = hash.match(/^#\/novel\/(.+)$/);
  if (m) return { kind: "novel", id: decodeURIComponent(m[1]) };
  return { kind: "page", path: PAGES[hash] ? hash : "#/shelf" };
}

async function route() {
  clearInterval(state.pollTimer);
  state.pollTimer = null;
  closeEvents();
  closeReader();

  const r = parseRoute();
  const activeNav = r.kind === "novel" ? "#/shelf" : r.path;
  $$(".nav-item").forEach((a) => a.classList.toggle("active", a.dataset.route === activeNav));
  $$(".view").forEach((v) => (v.hidden = true));

  if (r.kind === "novel") {
    $("#page-title").textContent = "本书工作台";
    $("#page-subtitle").textContent = "推进创作、查看章节与实时生成";
    $("#view-novel").hidden = false;
    try {
      await loadNovel(r.id);
    } catch (e) {
      toast("打开作品失败：" + e.message, true);
      location.hash = "#/shelf";
    }
    return;
  }

  const page = PAGES[r.path];
  $("#page-title").textContent = page.title;
  $("#page-subtitle").textContent = page.subtitle;
  $(page.view).hidden = false;

  if (r.path === "#/shelf") await loadShelf();
  if (r.path === "#/create") renderCreate();
  if (r.path === "#/runs") {
    await loadRuns();
    state.pollTimer = setInterval(() => loadRuns().catch(() => {}), 5000);
  }
  if (r.path === "#/settings") await loadSettingsForm();
}

window.addEventListener("hashchange", () => { route().catch((e) => toast(e.message, true)); });

/* ---------------- 书架 ---------------- */

async function loadShelf() {
  const data = await api("/api/novels");
  state.shelfItems = data.items || [];
  renderShelf();
}

function shelfFiltered(key) {
  const items = state.shelfItems;
  if (key === "active") return items.filter((i) => i.run_status === "running" || i.novel.status === "writing");
  if (key === "done") return items.filter((i) => i.novel.status === "drafted");
  if (key === "failed") return items.filter((i) => i.novel.status === "failed" || i.run_status === "failed");
  return items;
}

function renderShelf() {
  const defs = [["all", "全部"], ["active", "进行中"], ["done", "已完成"], ["failed", "异常"]];
  $("#shelf-filters").innerHTML = defs
    .map(([k, label]) => {
      const n = shelfFiltered(k).length;
      return `<span class="chip chip-filter ${state.shelfFilter === k ? "chip-dark" : ""}" data-filter="${k}">${label} ${n}</span>`;
    })
    .join("");
  const items = shelfFiltered(state.shelfFilter);
  const grid = $("#shelf-grid");
  if (!items.length) {
    const inner = state.shelfItems.length
      ? `<div class="empty-icon">🔍</div>
         <h3>没有符合条件的作品</h3>
         <p class="muted">试试切换上方的筛选条件。</p>`
      : `<div class="empty-icon">📚</div>
         <h3>书架还是空的</h3>
         <p class="muted">从一个想法开始：AI 会先生成立项方案与章节规划，再逐章写作、审校并保存。</p>
         <div class="row"><a class="btn btn-primary" href="#/create">创建新小说</a></div>`;
    grid.innerHTML = `<div class="novel-card empty-card">${inner}</div>`;
    return;
  }

  grid.innerHTML = items.map((it, i) => {
    const n = it.novel;
    const target = it.chapters_target || 0;
    const done = it.chapters_done || 0;
    const pct = target > 0 ? Math.min(100, Math.round((done / target) * 100)) : 0;
    const title = (n.title && n.title.trim()) || (n.idea || "").slice(0, 16) || "（未命名）";
    return `
    <article class="novel-card" data-id="${esc(n.id)}" style="--i:${i}">
      <div class="novel-card-head">
        <h3>${esc(title)}</h3>
        <div class="chips">
          ${n.genre ? `<span class="chip">${esc(n.genre)}</span>` : ""}
          ${statusChip(n.status)}
        </div>
      </div>
      <p class="novel-card-sum">${esc(n.logline || n.idea || "")}</p>
      <p class="meta">已完成 ${done} 章 · 共 ${it.words || 0} 字 · 更新于 ${fmtTime(n.updated_at)}</p>
      <div class="status-box">
        <div class="chips">
          ${it.run_status ? `<span class="chip chip-dark">${esc(RUN_STATUS[it.run_status] || it.run_status)}</span>` : `<span class="chip">尚无运行记录</span>`}
          ${target > 0 ? `<span class="chip">进度 ${pct}%</span>` : ""}
          ${it.run_stage ? `<span class="chip">当前阶段：${esc(STAGE_LABEL[it.run_stage] || it.run_stage)}</span>` : ""}
        </div>
        <div class="progress"><div class="progress-fill" style="width:${pct}%"></div></div>
        ${it.last_error ? `<p class="err">${esc(it.last_error)}</p>` : ""}
      </div>
      <div class="row">
        <button class="btn btn-primary btn-sm" data-act="open">进入工作台</button>
        <a class="btn btn-outline btn-sm" data-act="export" href="/api/novels/${esc(n.id)}/export">导出 TXT</a>
        <button class="btn btn-ghost btn-sm btn-danger" data-act="delete">删除</button>
      </div>
    </article>`;
  }).join("");
}

function bindShelf() {
  $("#shelf-grid").addEventListener("click", async (e) => {
    const card = e.target.closest(".novel-card");
    const act = e.target.closest("[data-act]");
    if (!card || !act) return;
    const id = card.dataset.id;
    const kind = act.dataset.act;
    if (kind === "open") {
      location.hash = `#/novel/${id}`;
      return;
    }
    if (kind === "delete") {
      e.preventDefault();
      const title = card.querySelector("h3").textContent;
      if (!confirm(`确定删除《${title}》？该操作会删除全部章节与运行记录，且不可恢复。`)) return;
      try {
        await api(`/api/novels/${id}`, { method: "DELETE" });
        toast("已删除");
        await loadShelf();
      } catch (err2) {
        toast(err2.message, true);
      }
    }
  });
  $("#shelf-filters").addEventListener("click", (e) => {
    const chip = e.target.closest("[data-filter]");
    if (!chip) return;
    state.shelfFilter = chip.dataset.filter;
    renderShelf();
  });
  $("#shelf-create").onclick = () => { location.hash = "#/create"; };
  $("#shelf-refresh").onclick = () => loadShelf().catch((e) => toast(e.message, true));
}

/* ---------------- 新建作品 ---------------- */

function renderCreate() {
  const provider = state.settings?.provider;
  const model = provider === "mock" ? "离线演示（mock）" : state.settings?.model || "未配置";
  $("#create-model").textContent = model;
}

function bindCreate() {
  $("#create-form").addEventListener("submit", async (e) => {
    e.preventDefault();
    const idea = $("#create-idea").value.trim();
    if (!idea) { toast("请先写一句灵感", true); return; }
    const chapters = Math.min(50, Math.max(1, parseInt($("#create-chapters").value || "3", 10) || 3));
    const btn = $("#create-submit");
    btn.disabled = true;
    btn.textContent = "正在创建…";
    try {
      const r = await api("/api/novels", {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({ idea, chapters }),
      });
      toast("已开始创作，进入工作台查看进度");
      location.hash = `#/novel/${r.novel_id}`;
    } catch (err) {
      toast(err.message, true);
    } finally {
      btn.disabled = false;
      btn.textContent = "开始创作";
    }
  });
}

/* ---------------- 作品工作台 ---------------- */

async function loadNovel(id) {
  const data = await api(`/api/novels/${id}`);
  state.novelId = id;
  state.novel = data;
  renderNovel();
  subscribe(id);
}

function computeSteps(data) {
  const n = data.novel || {};
  const chs = data.chapters || [];
  const run = data.run;
  const target = (run && run.chapters_target) || chs.length || 0;
  const briefDone = !!(n.title && n.title.trim());
  const outlineDone = chs.length > 0;
  const drafted = chs.filter((c) => (c.words || 0) > 0).length;
  const draftDone = outlineDone && target > 0 && drafted >= target;
  const reviewed = chs.filter((c) => c.review).length;
  const reviewDone = draftDone && drafted > 0 && reviewed >= drafted;
  const finished = !!(run && run.status === "done");
  const running = !!(run && run.status === "running");

  const steps = [
    { name: "整理立项方案", done: briefDone },
    { name: "生成章节规划", done: outlineDone },
    { name: "写作章节正文", done: draftDone },
    { name: "审校并保存", done: reviewDone || finished },
  ];
  let activeIdx = -1;
  if (running) activeIdx = steps.findIndex((s) => !s.done);
  return steps.map((s, i) => ({ ...s, state: s.done ? "done" : i === activeIdx ? "active" : "pending" }));
}

function renderNovel() {
  const data = state.novel;
  if (!data) return;
  const n = data.novel;
  const chs = data.chapters || [];
  const run = data.run;

  $("#novel-title").textContent = (n.title && n.title.trim()) || "（未命名）";
  $("#novel-chips").innerHTML = [
    n.genre ? `<span class="chip">${esc(n.genre)}</span>` : "",
    statusChip(n.status),
    run ? `<span class="chip chip-dark">${esc(RUN_STATUS[run.status] || run.status)}</span>` : "",
  ].join("");
  $("#novel-meta").textContent = [n.logline, n.protagonist && `主角：${n.protagonist}`, n.style].filter(Boolean).join(" · ") || n.idea || "";

  const target = (run && run.chapters_target) || Math.max(chs.length, 3);
  if (!$("#novel-target").dataset.touched) {
    $("#novel-target").value = Math.max(chs.length, target);
  }
  $("#novel-export").href = `/api/novels/${n.id}/export`;
  const running = !!(run && run.status === "running");
  const runBtn = $("#novel-run");
  runBtn.disabled = running;
  runBtn.textContent = running ? "创作中…" : "继续创作";

  // 进度
  const done = chs.filter((c) => c.status === "done").length;
  const pct = target > 0 ? Math.min(100, Math.round((done / target) * 100)) : 0;
  $("#run-chips").innerHTML = [
    run ? `<span class="chip ${run.status === "failed" ? "chip-danger" : run.status === "done" ? "chip-success" : "chip-info"}">${esc(RUN_STATUS[run.status] || run.status)}</span>` : `<span class="chip">尚未运行</span>`,
    `<span class="chip">进度 ${done}/${target} 章</span>`,
    run ? `<span class="chip">当前阶段：${esc(STAGE_LABEL[run.stage] || run.stage)}</span>` : "",
  ].join("");
  $("#run-progress").style.width = pct + "%";
  $("#run-progress").classList.toggle("running", running);
  $("#run-note").textContent = run && run.error
    ? "已中断：" + run.error
    : run && run.status === "running"
      ? "AI 正在按「立项 → 规划 → 正文 → 审校」推进，可离开本页，任务在后台继续。"
      : run && run.status === "done"
        ? `最近一次运行已完成，共 ${done} 章。`
        : "点击「继续创作」开始或推进本书。";

  // 步骤条
  $("#novel-steps").innerHTML = computeSteps(data).map((s, i) => `
    <div class="step ${s.state === "done" ? "step-done" : s.state === "active" ? "step-active" : ""}">
      <span class="step-num">${s.state === "done" ? "✓" : i + 1}</span>
      <span class="step-name">${esc(s.name)}</span>
      <span class="step-state">${s.state === "done" ? "已完成" : s.state === "active" ? "进行中" : "待推进"}</span>
    </div>`).join("");

  // 章节表
  const tbody = $("#chapter-rows");
  tbody.innerHTML = chs.length
    ? chs.map((c) => `
      <tr>
        <td class="num">${c.idx}</td>
        <td>${esc(c.title || `第${c.idx}章`)}</td>
        <td>${c.status === "done" ? `<span class="chip chip-success">已完成</span>` : c.status === "drafted" ? `<span class="chip chip-info">待审校</span>` : `<span class="chip">已规划</span>`}</td>
        <td class="num">${c.words || 0}</td>
        <td>${reviewChip(c.review)}</td>
        <td><button class="btn btn-outline btn-sm" data-read="${c.idx}">阅读</button></td>
      </tr>`).join("")
    : `<tr><td colspan="6" class="muted">章节规划生成后会显示在这里。</td></tr>`;
}

function bindNovel() {
  $("#chapter-rows").addEventListener("click", (e) => {
    const btn = e.target.closest("[data-read]");
    if (!btn) return;
    openReader(parseInt(btn.dataset.read, 10)).catch((err) => toast(err.message, true));
  });
  $("#novel-target").addEventListener("input", (e) => { e.target.dataset.touched = "1"; });
  $("#novel-run").onclick = async () => {
    const chapters = Math.min(50, Math.max(1, parseInt($("#novel-target").value || "3", 10) || 3));
    try {
      await api(`/api/novels/${state.novelId}/run`, {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({ chapters }),
      });
      toast(`已开始，目标 ${chapters} 章（已完成章节会自动跳过）`);
      $("#novel-run").disabled = true;
      $("#novel-run").textContent = "创作中…";
    } catch (err) {
      toast(err.message, true);
      refreshNovel();
    }
  };
  $("#novel-delete").onclick = async () => {
    const title = $("#novel-title").textContent;
    if (!confirm(`确定删除《${title}》？全部章节与运行记录都会被删除。`)) return;
    try {
      await api(`/api/novels/${state.novelId}`, { method: "DELETE" });
      toast("已删除");
      location.hash = "#/shelf";
    } catch (err) {
      toast(err.message, true);
    }
  };
  $("#stream-clear").onclick = () => {
    state.stream = { chapter: null, text: "" };
    const pre = $("#stream-pre");
    pre.textContent = "等待生成…";
    pre.classList.add("muted");
    $("#stream-live").classList.remove("on");
  };
}

/* ---------------- 阅读弹层 ---------------- */

async function openReader(idx) {
  const c = await api(`/api/novels/${state.novelId}/chapters/${idx}`);
  $("#reader-title").textContent = `第${c.idx}章 ${c.title || ""}`;
  const review = parseReview(c.review);
  $("#reader-review").innerHTML = review
    ? review.issues.length
      ? review.issues.map((it) => `<span class="chip ${it.severity === "high" ? "chip-danger" : it.severity === "medium" ? "chip-warning" : "chip"}">${esc(it.note || it.severity)}</span>`).join("")
      : `<span class="chip chip-success">审校通过</span>`
    : "";
  $("#reader-goal").textContent = [
    c.goal ? `本章目标：${c.goal}` : "",
    c.words ? `字数：${c.words}` : "",
  ].filter(Boolean).join(" · ");
  $("#reader-content").textContent = c.content || "（本章还没有正文）";
  $("#reader-backdrop").hidden = false;
}

function closeReader() {
  $("#reader-backdrop").hidden = true;
}

/* ---------------- 实时事件 ---------------- */

function closeEvents() {
  if (state.es) {
    state.es.close();
    state.es = null;
  }
}

function subscribe(id) {
  closeEvents();
  state.stream = { chapter: null, text: "" };
  $("#novel-log").innerHTML = "";
  const preStream = $("#stream-pre");
  preStream.textContent = "等待生成…";
  preStream.classList.add("muted");
  $("#stream-live").classList.remove("on");
  const es = new EventSource(`/api/novels/${id}/events`);
  state.es = es;
  es.addEventListener("engine", (e) => {
    let ev;
    try { ev = JSON.parse(e.data); } catch { return; }
    handleEvent(ev);
  });
}

function novelLog(line) {
  const el = $("#novel-log");
  const div = document.createElement("div");
  div.className = "line";
  div.textContent = `${new Date().toLocaleTimeString()} · ${line}`;
  el.appendChild(div);
  el.scrollTop = el.scrollHeight;
}

function handleEvent(ev) {
  switch (ev.type) {
    case "snapshot":
      if (ev.run) novelLog(`已连接 · 当前运行：${RUN_STATUS[ev.run.status] || ev.run.status} / ${STAGE_LABEL[ev.run.stage] || ev.run.stage}`);
      break;
    case "run_started":
      novelLog(`运行开始（目标 ${ev.chapters} 章）`);
      break;
    case "stage":
      novelLog(ev.message);
      break;
    case "token": {
      const pre = $("#stream-pre");
      pre.classList.remove("muted");
      $("#stream-live").classList.add("on");
      if (state.stream.chapter !== ev.chapter_idx) {
        state.stream.chapter = ev.chapter_idx;
        state.stream.text += `${state.stream.text ? "\n\n" : ""}—— 第${ev.chapter_idx}章 ——\n`;
      }
      state.stream.text += ev.delta;
      pre.textContent = state.stream.text;
      pre.scrollTop = pre.scrollHeight;
      break;
    }
    case "chapter_saved":
      novelLog(`第${ev.chapter_idx}章已保存：${ev.title}（${ev.words} 字）`);
      refreshNovel();
      break;
    case "chapter_review":
      novelLog(`第${ev.chapter_idx}章审校：${ev.ok ? "通过" : "有提醒"}（${ev.issues} 条）`);
      break;
    case "run_finished":
      novelLog("本次运行完成");
      $("#stream-live").classList.remove("on");
      refreshNovel();
      break;
    case "run_failed":
      novelLog(`运行失败：${ev.error}`);
      $("#stream-live").classList.remove("on");
      refreshNovel();
      break;
  }
}

async function refreshNovel() {
  if (!state.novelId) return;
  try {
    state.novel = await api(`/api/novels/${state.novelId}`);
    if (parseRoute().kind === "novel") renderNovel();
  } catch { /* 忽略刷新失败 */ }
}

/* ---------------- 运行记录 ---------------- */

async function loadRuns() {
  const data = await api("/api/runs?limit=50");
  const items = data.items || [];
  $("#runs-rows").innerHTML = items.length
    ? items.map((it) => {
      const r = it.run;
      const pct = r.chapters_target > 0 ? Math.min(100, Math.round(((r.chapter_cursor || 0) / r.chapters_target) * 100)) : 0;
      return `
      <tr>
        <td class="num">${fmtTime(r.created_at)}</td>
        <td><a href="#/novel/${esc(r.novel_id)}">${esc(it.novel_title || "（未命名）")}</a></td>
        <td class="num">${r.chapters_target} 章</td>
        <td class="num">${r.chapter_cursor || 0} 章 · ${pct}%</td>
        <td>${r.status === "done" ? `<span class="chip chip-success">已完成</span>` : r.status === "failed" ? `<span class="chip chip-danger">失败</span>` : `<span class="chip chip-info">执行中</span>`}</td>
        <td>${esc(STAGE_LABEL[r.stage] || r.stage)}</td>
        <td class="muted">${r.error ? esc(r.error) : "—"}</td>
      </tr>`;
    }).join("")
    : `<tr><td colspan="7" class="muted">还没有运行记录。</td></tr>`;
}

/* ---------------- 设置 ---------------- */

const PRESETS = {
  deepseek: { base: "https://api.deepseek.com/v1", model: "deepseek-chat" },
  qwen: { base: "https://dashscope.aliyuncs.com/compatible-mode/v1", model: "qwen-plus" },
  glm: { base: "https://open.bigmodel.cn/api/paas/v4", model: "glm-4.5-air" },
  ollama: { base: "http://127.0.0.1:11434/v1", model: "qwen2.5:7b" },
};

async function loadSettings() {
  state.settings = await api("/api/settings/llm");
  renderModelChip();
}

function renderModelChip() {
  const s = state.settings;
  const label = !s ? "模型：—" : s.provider === "mock" ? "模型：离线演示" : `模型：${s.model || "未配置"}`;
  $("#model-chip").textContent = label;
}

function syncProviderFields() {
  const isMock = $("#set-provider").value === "mock";
  ["#set-base", "#set-key", "#set-model", "#set-temp"].forEach((sel) => {
    $(sel).disabled = isMock;
  });
}

async function loadSettingsForm() {
  await loadSettings();
  const s = state.settings;
  $("#set-provider").value = s.provider || "mock";
  $("#set-base").value = s.base_url || "";
  $("#set-key").value = s.api_key || "";
  $("#set-model").value = s.model || "";
  $("#set-temp").value = typeof s.temperature === "number" ? Math.round(s.temperature * 100) / 100 : 0.8;
  syncProviderFields();
  $("#set-status").textContent = "";
}

function bindSettings() {
  $("#settings-form").addEventListener("submit", async (e) => {
    e.preventDefault();
    const body = {
      provider: $("#set-provider").value,
      base_url: $("#set-base").value.trim(),
      api_key: $("#set-key").value.trim(),
      model: $("#set-model").value.trim(),
      temperature: parseFloat($("#set-temp").value || "0.8"),
    };
    try {
      await api("/api/settings/llm", {
        method: "PUT",
        headers: { "content-type": "application/json" },
        body: JSON.stringify(body),
      });
      state.settings = body;
      renderModelChip();
      $("#set-status").textContent = "已保存";
      setTimeout(() => ($("#set-status").textContent = ""), 2000);
    } catch (err) {
      toast(err.message, true);
    }
  });

  $("#set-provider").addEventListener("change", syncProviderFields);

  $("#set-test").onclick = async () => {
    $("#set-status").textContent = "测试中…";
    try {
      const r = await api("/api/settings/llm/test", { method: "POST" });
      $("#set-status").textContent = r.ok ? `连接正常：${r.message}` : `连接失败：${r.message}`;
    } catch (err) {
      $("#set-status").textContent = "测试失败：" + err.message;
    }
  };

  $$(".presets [data-preset]").forEach((btn) => {
    btn.onclick = () => {
      const p = PRESETS[btn.dataset.preset];
      if (!p) return;
      $("#set-provider").value = "openai";
      $("#set-base").value = p.base;
      $("#set-model").value = p.model;
    };
  });
}

/* ---------------- 主题 / 引擎状态 ---------------- */

function bindTheme() {
  const saved = localStorage.getItem("ainovel.theme");
  const prefersDark = window.matchMedia && window.matchMedia("(prefers-color-scheme: dark)").matches;
  const dark = saved ? saved === "dark" : prefersDark;
  document.documentElement.classList.toggle("dark", dark);
  $("#theme-toggle").onclick = () => {
    const next = !document.documentElement.classList.contains("dark");
    document.documentElement.classList.toggle("dark", next);
    localStorage.setItem("ainovel.theme", next ? "dark" : "light");
  };
}

async function loadHealth() {
  try {
    const h = await api("/api/health");
    $("#engine-dot").className = "dot ok";
    $("#engine-status").textContent = `引擎运行中 · v${h.version}`;
  } catch {
    $("#engine-dot").className = "dot bad";
    $("#engine-status").textContent = "引擎未连接";
  }
}

/* ---------------- 启动 ---------------- */

function init() {
  bindTheme();
  bindShelf();
  bindCreate();
  bindNovel();
  bindSettings();
  $("#reader-close").onclick = closeReader;
  $("#reader-backdrop").addEventListener("click", (e) => {
    if (e.target === $("#reader-backdrop")) closeReader();
  });
  loadSettings().catch(() => {});
  loadHealth();
  setInterval(loadHealth, 15000);
  route().catch((e) => toast(e.message, true));
}

init();
