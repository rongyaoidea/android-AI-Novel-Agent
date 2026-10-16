"use strict";

/* ============================================================
 * android-AI-Novel-Agent · Web UI
 * 书架 / 新建作品 / 本书工作台（章节·角色·伏笔） / 运行记录 / 模型设置
 * 章节编辑器：手动修改 + 选段 AI 修文（改写/扩写/精简/去 AI 味）
 * 按阶段确认：规划后暂停，可改章纲再继续
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
  tab: "chapters",
  editorDirty: false,
  editorIssues: [],
  polishRange: null,
  polishResult: null,
  polishLast: null,
  gen: null,
  health: null,
  foreshadowings: [],
  styles: null,
  promptTask: null,
  shelfSort: "updated",
  shelfQuery: "",
};

/* ---------------- 工具 ---------------- */

function esc(v) {
  return String(v ?? "").replace(/[&<>"']/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" }[c]));
}

/** 手绘线条图标（引用 index.html 中的 SVG sprite） */
function ic(name, cls = "ic") {
  return `<svg class="${cls}"><use href="#${name}"/></svg>`;
}

/* ---------------- 本地访问令牌 ----------------
 * 来源优先级：URL ?token=（Android 壳注入）→ sessionStorage 暂存。
 * 之后所有 /api 请求带 x-ainovel-token 头；EventSource / 下载链接走 ?token=。
 * 令牌只在本机会话里流转，不写入 localStorage，也不回写地址栏。
 */
const TOKEN_KEY = "ainovel.token";
const AUTH_TOKEN = (() => {
  try {
    const t = new URLSearchParams(location.search).get("token");
    if (t) {
      sessionStorage.setItem(TOKEN_KEY, t);
      const clean = location.pathname + (location.hash || "#/shelf");
      history.replaceState(null, "", clean);
      return t;
    }
    return sessionStorage.getItem(TOKEN_KEY) || "";
  } catch {
    return "";
  }
})();

/** 给无法加请求头的 URL 追加令牌（EventSource、a[download]） */
function withToken(path) {
  if (!AUTH_TOKEN) return path;
  return path + (path.includes("?") ? "&" : "?") + "token=" + encodeURIComponent(AUTH_TOKEN);
}

async function api(path, opts) {
  const o = Object.assign({}, opts);
  o.headers = Object.assign({}, o.headers);
  if (AUTH_TOKEN) o.headers["x-ainovel-token"] = AUTH_TOKEN;
  const r = await fetch(path, o);
  if (!r.ok) throw new Error((await r.text()) || r.statusText);
  return r.json();
}

async function apiSend(path, method, body) {
  return api(path, {
    method,
    headers: { "content-type": "application/json" },
    body: JSON.stringify(body),
  });
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
  await_confirm: { label: "待确认", cls: "chip-warning" },
  drafted: { label: "已完成", cls: "chip-success" },
  failed: { label: "异常", cls: "chip-danger" },
};
const RUN_STATUS = { running: "执行中", paused: "待确认", done: "已完成", failed: "失败" };
const STAGE_LABEL = {
  brief: "整理立项",
  outline: "章节规划",
  await_confirm: "待确认",
  draft: "正文生成",
  review: "章节审校",
  done: "全部完成",
  failed: "已中断",
};
const CHARACTER_STATUS = { active: "在场", dead: "死亡", leave: "离场" };

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
  closeDiff();
  closeForm();
  closeGen();

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
  if (r.path === "#/create") await renderCreate();
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
  let items = state.shelfItems;
  if (key === "active") {
    items = items.filter((i) => i.run_status === "running" || i.novel.status === "writing" || i.novel.status === "await_confirm");
  } else if (key === "done") {
    items = items.filter((i) => i.novel.status === "drafted");
  } else if (key === "failed") {
    items = items.filter((i) => i.novel.status === "failed" || i.run_status === "failed");
  }
  const q = (state.shelfQuery || "").trim().toLowerCase();
  if (q) {
    items = items.filter((i) => {
      const n = i.novel;
      return [n.title, n.idea, n.logline].filter(Boolean).join(" ").toLowerCase().includes(q);
    });
  }
  const sorted = items.slice();
  const sort = state.shelfSort || "updated";
  if (sort === "words") {
    sorted.sort((a, b) => (b.words || 0) - (a.words || 0));
  } else if (sort === "progress") {
    const pct = (i) => (i.chapters_target ? (i.chapters_done || 0) / i.chapters_target : 0);
    sorted.sort((a, b) => pct(b) - pct(a));
  } else {
    sorted.sort((a, b) => String(b.novel.updated_at || "").localeCompare(String(a.novel.updated_at || "")));
  }
  return sorted;
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
      ? `<div class="empty-icon">${ic("i-search", "ic-xl")}</div>
         <h3>没有符合条件的作品</h3>
         <p class="muted">试试切换上方的筛选条件。</p>`
      : `<div class="empty-icon">${ic("i-book", "ic-xl")}</div>
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
          ${n.mode === "staged" ? `<span class="chip">按阶段确认</span>` : ""}
          ${statusChip(n.status)}
        </div>
      </div>
      <p class="novel-card-sum">${esc(n.logline || n.idea || "")}</p>
      <p class="meta">已完成 ${done} 章 · 共 ${it.words || 0} 字 · 更新于 ${fmtTime(n.updated_at)}</p>
      <div class="status-box">
        <div class="chips">
          ${it.run_status ? `<span class="chip ${it.run_status === "paused" ? "chip-warning" : "chip-dark"}">${esc(RUN_STATUS[it.run_status] || it.run_status)}</span>` : `<span class="chip">尚无运行记录</span>`}
          ${target > 0 ? `<span class="chip">进度 ${pct}%</span>` : ""}
          ${it.run_stage ? `<span class="chip">当前阶段：${esc(STAGE_LABEL[it.run_stage] || it.run_stage)}</span>` : ""}
        </div>
        <div class="progress"><div class="progress-fill ${it.run_status === "running" ? "running" : ""}" style="width:${pct}%"></div></div>
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
  $("#shelf-search").addEventListener("input", (e) => {
    state.shelfQuery = e.target.value;
    renderShelf();
  });
  $("#shelf-sort").addEventListener("change", (e) => {
    state.shelfSort = e.target.value;
    renderShelf();
  });
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
      if (!confirm(`确定删除《${title}》？该操作会删除全部章节、台账与运行记录，且不可恢复。`)) return;
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

async function renderCreate() {
  const provider = state.settings?.provider;
  const model = provider === "mock" ? "离线演示（mock）" : state.settings?.model || "未配置";
  $("#create-hint").textContent = `当前模型：${model}（可在「模型设置」中调整）`;
  try {
    await loadStyles();
  } catch {
    /* 目录拉取失败时静默：不影响建书 */
  }
  mountStylePicker(
    $("#create-style-grid"),
    $("#create-style-groups"),
    getStylePick(),
    (id) => setStylePick(id),
  );
}

function bindCreate() {
  $("#create-form").addEventListener("submit", async (e) => {
    e.preventDefault();
    const idea = $("#create-idea").value.trim();
    if (!idea) { toast("请先写一句灵感", true); return; }
    const chapters = Math.min(50, Math.max(1, parseInt($("#create-chapters").value || "3", 10) || 3));
    const mode = $("#create-mode").value;
    const style_preset = getStylePick();
    const btn = $("#create-submit");
    btn.disabled = true;
    btn.textContent = "正在创建…";
    try {
      const r = await apiSend("/api/novels", "POST", { idea, chapters, mode, style_preset });
      toast(mode === "staged" ? "已开始：规划完成后会暂停等你确认" : "已开始创作，进入工作台查看进度");
      location.hash = `#/novel/${r.novel_id}`;
    } catch (err) {
      toast(err.message, true);
    } finally {
      btn.disabled = false;
      btn.textContent = "开始创作";
    }
  });
}

/* ---------------- 文风预设 ---------------- */

const STYLE_GROUPS = ["全部", "男频", "女频", "通用"];
const STYLE_PICK_KEY = "ainovel.style";

async function loadStyles() {
  if (!state.styles) {
    const r = await api("/api/styles");
    state.styles = r.items || [];
  }
  return state.styles;
}

function styleById(id) {
  return (state.styles || []).find((s) => s.id === id) || null;
}

function styleName(id) {
  const s = styleById(id);
  return s ? s.name : "不限";
}

function getStylePick() {
  try {
    return localStorage.getItem(STYLE_PICK_KEY) || "";
  } catch {
    return "";
  }
}

function setStylePick(id) {
  try {
    localStorage.setItem(STYLE_PICK_KEY, id);
  } catch {
    /* 忽略 */
  }
}

/**
 * 渲染文风选择器（新建页与工作台弹层共用）：
 * grid/groups 为容器；selectedId 为空串表示「不限」；onPick(id) 在点选后回调。
 */
function mountStylePicker(grid, groups, selectedId, onPick) {
  let group = "全部";
  const draw = () => {
    groups.innerHTML = STYLE_GROUPS.map(
      (g) => `<span class="chip chip-filter ${g === group ? "chip-dark" : ""}" data-sgroup="${g}">${g}</span>`,
    ).join("");
    const list = state.styles || [];
    const card = (id, name, badge, tagline, genres, traits) => `
      <div class="style-card ${id === selectedId ? "selected" : ""}" data-style="${id}">
        <div class="style-card-head">
          <h4>${esc(name)}</h4>
          ${badge ? `<span class="chip">${esc(badge)}</span>` : ""}
        </div>
        <p class="style-tagline">${esc(tagline)}</p>
        ${genres ? `<p class="style-genres">${esc(genres)}</p>` : ""}
        <div class="chips">${(traits || []).slice(0, 3).map((t) => `<span class="chip">${esc(t[0])} · ${esc(t[1])}</span>`).join("")}</div>
      </div>`;
    const cards = [card("", "不限", "", "跟随模型自由发挥（默认写法）", "", [])];
    for (const s of list) {
      if (group !== "全部" && s.group !== group) continue;
      cards.push(card(s.id, s.name, s.group, s.tagline, s.genres, s.traits));
    }
    grid.innerHTML = cards.join("");
  };
  groups.onclick = (e) => {
    const g = e.target.closest("[data-sgroup]");
    if (!g) return;
    group = g.dataset.sgroup;
    draw();
  };
  grid.onclick = (e) => {
    const c = e.target.closest("[data-style]");
    if (!c) return;
    selectedId = c.dataset.style;
    draw();
    if (onPick) onPick(selectedId);
  };
  draw();
}

/** 工作台：更换文风预设（选择即 PATCH 保存） */
async function openStylePicker() {
  try {
    await loadStyles();
  } catch (err) {
    toast(err.message, true);
    return;
  }
  const current = state.novel?.novel?.style_preset || "";
  mountStylePicker($("#style-grid"), $("#style-groups"), current, async (id) => {
    try {
      await apiSend(`/api/novels/${state.novelId}`, "PATCH", { style_preset: id });
      if (state.novel?.novel) state.novel.novel.style_preset = id;
      renderNovel();
      toast(id ? `已切换文风：${styleName(id)}（对之后生成的内容生效）` : "已改为不限文风");
      $("#style-backdrop").hidden = true;
    } catch (err) {
      toast(err.message, true);
    }
  });
  $("#style-backdrop").hidden = false;
}

/* ---------------- 本书工作台 ---------------- */

async function loadNovel(id) {
  const data = await api(`/api/novels/${id}`);
  state.novelId = id;
  state.novel = data;
  try {
    await loadStyles();
  } catch {
    /* 目录拉取失败时静默：文风 chip 显示为「不限」 */
  }
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
  const paused = !!(run && run.status === "paused");

  const steps = [
    { name: "整理立项方案", done: briefDone },
    { name: "生成章节规划", done: outlineDone },
    { name: "写作章节正文", done: draftDone },
    { name: "审校并保存", done: reviewDone || finished },
  ];
  let activeIdx = -1;
  if (running || paused) activeIdx = steps.findIndex((s) => !s.done);
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
    run ? `<span class="chip ${run.status === "failed" ? "chip-danger" : run.status === "paused" ? "chip-warning" : run.status === "done" ? "chip-success" : "chip-info"}">${esc(RUN_STATUS[run.status] || run.status)}</span>` : "",
    `<span class="chip chip-filter ${n.style_preset ? "" : "muted-chip"}" id="novel-style-chip" title="点击更换文风预设">${ic("i-pen")}文风：${esc(styleName(n.style_preset))}</span>`,
  ].join("");
  $("#novel-meta").textContent = [n.logline, n.protagonist && `主角：${n.protagonist}`, n.style].filter(Boolean).join(" · ") || n.idea || "";

  const target = (run && run.chapters_target) || Math.max(chs.length, 3);
  if (!$("#novel-target").dataset.touched) {
    $("#novel-target").value = Math.max(chs.length, target);
  }
  $("#novel-export").href = withToken(`/api/novels/${n.id}/export`);

  const running = !!(run && run.status === "running");
  const paused = !!(run && run.status === "paused");
  const runBtn = $("#novel-run");
  runBtn.disabled = running;
  runBtn.textContent = running ? "创作中…" : paused ? "确认并继续" : "继续创作";
  runBtn.classList.toggle("attention", paused);

  // 推进方式切换
  $("#mode-toggle").innerHTML = `
    <span class="chip chip-filter ${n.mode === "auto" ? "chip-dark" : ""}" data-mode="auto" title="全自动推进">全自动</span>
    <span class="chip chip-filter ${n.mode === "staged" ? "chip-dark" : ""}" data-mode="staged" title="规划后暂停确认">阶段确认</span>`;

  // 进度
  const done = chs.filter((c) => c.status === "done").length;
  const pct = target > 0 ? Math.min(100, Math.round((done / target) * 100)) : 0;
  $("#run-chips").innerHTML = [
    run ? `<span class="chip ${run.status === "failed" ? "chip-danger" : run.status === "paused" ? "chip-warning" : run.status === "done" ? "chip-success" : "chip-info"}">${esc(RUN_STATUS[run.status] || run.status)}</span>` : `<span class="chip">尚未运行</span>`,
    `<span class="chip">进度 ${done}/${target} 章</span>`,
    run ? `<span class="chip">当前阶段：${esc(STAGE_LABEL[run.stage] || run.stage)}</span>` : "",
  ].join("");
  $("#run-progress").style.width = pct + "%";
  $("#run-progress").classList.toggle("running", running);
  $("#run-note").textContent = run && run.error
    ? "已中断：" + run.error
    : paused
      ? "已暂停：章节规划已生成。到下方「章节」里检查/修改章纲，然后点「确认并继续」开始写正文。"
      : running
        ? "AI 正在按「立项 → 规划 → 正文 → 审校」推进，可离开本页，任务在后台继续。"
        : run && run.status === "done"
          ? `最近一次运行已完成，共 ${done} 章。`
          : "点击「继续创作」开始或推进本书。";

  // 步骤条
  $("#novel-steps").innerHTML = computeSteps(data).map((s, i) => `
    <div class="step ${s.state === "done" ? "step-done" : s.state === "active" ? "step-active" : ""}">
      <span class="step-num">${s.state === "done" ? "✓" : i + 1}</span>
      <span class="step-name">${esc(s.name)}</span>
      <span class="step-state">${s.state === "done" ? "已完成" : s.state === "active" ? (paused ? "等待确认" : "进行中") : "待推进"}</span>
    </div>`).join("");

  // 当前页签内容
  renderTab();

  // 页签高亮
  $$("#novel-tabs [data-tab]").forEach((el) => {
    el.classList.toggle("chip-dark", el.dataset.tab === state.tab);
  });

  refreshInsights();
}

function renderTab() {
  $("#tab-chapters").hidden = state.tab !== "chapters";
  $("#tab-characters").hidden = state.tab !== "characters";
  $("#tab-foreshadowings").hidden = state.tab !== "foreshadowings";
  if (state.tab === "chapters") renderChapters();
  if (state.tab === "characters") renderCharacters().catch((e) => toast(e.message, true));
  if (state.tab === "foreshadowings") renderForeshadowings().catch((e) => toast(e.message, true));
}

/** 头部思维提示：未回收伏笔数 + 有审校提醒的章节数（可点击直达页签） */
async function refreshInsights() {
  if (!state.novelId) return;
  let openF = 0;
  try {
    const f = await api(`/api/novels/${state.novelId}/foreshadowings`);
    state.foreshadowings = f.items || [];
    openF = state.foreshadowings.filter((x) => x.status === "open").length;
  } catch { /* 忽略 */ }
  if (parseRoute().kind !== "novel") return;
  const issues = (state.novel?.chapters || []).filter((c) => {
    const r = parseReview(c.review);
    return r && (!r.ok || r.issues.length > 0);
  }).length;
  const el = $("#novel-insights");
  if (!el) return;
  el.innerHTML =
    `<span class="chip chip-filter ${openF ? "chip-warning" : ""}" data-insight="foreshadowings" title="点击查看伏笔台账">${ic("i-flag")}未回收伏笔 ${openF}</span>` +
    `<span class="chip chip-filter ${issues ? "chip-warning" : ""}" data-insight="chapters" title="点击查看章节">${ic("i-flag")}审校提醒 ${issues} 章</span>`;
}

/* ---------------- 书籍体检 ---------------- */

async function openHealth() {
  const h = await api(`/api/novels/${state.novelId}/health`);
  state.health = h;
  $("#health-stats").innerHTML = [
    `<span class="chip">章节 ${h.chapters_done}/${h.chapters_total}</span>`,
    `<span class="chip">角色 ${h.characters}</span>`,
    `<span class="chip ${h.open_foreshadowings ? "chip-warning" : ""}">未回收伏笔 ${h.open_foreshadowings}</span>`,
    `<span class="chip ${h.review_warnings ? "chip-warning" : ""}">审校意见 ${h.review_warnings}</span>`,
    `<span class="chip ${h.missing_summary.length ? "chip-warning" : ""}">缺摘要 ${h.missing_summary.length}</span>`,
    `<span class="chip ${h.missing_content.length ? "chip-warning" : ""}">未完正文 ${h.missing_content.length}</span>`,
  ].join("");
  $("#health-suggestions").innerHTML = (h.suggestions || [])
    .map((s) => `<p class="health-line">${ic("i-flag")}<span>${esc(s)}</span></p>`)
    .join("");
  $("#health-status").textContent = "";
  $("#health-backdrop").hidden = false;
}

async function fillMissing() {
  const h = state.health;
  if (!h) return;
  const ids = (h.missing_summary || []).slice();
  if (!ids.length) {
    $("#health-status").textContent = "没有需要补齐的章节";
    return;
  }
  const btn = $("#health-fill");
  btn.disabled = true;
  try {
    for (let i = 0; i < ids.length; i++) {
      $("#health-status").textContent = `补齐资料 ${i + 1}/${ids.length}…`;
      await api(`/api/novels/${state.novelId}/chapters/${ids[i]}/extract`, { method: "POST" });
    }
    toast("资料已补齐");
    await refreshNovel();
    await openHealth();
  } catch (err) {
    toast(err.message, true);
  } finally {
    btn.disabled = false;
  }
}

/* ---------------- AI 用量 ---------------- */

const USAGE_TASK_LABEL = {
  brief: "立项方案",
  outline: "章节规划",
  chapter: "正文生成",
  chapter_retry: "文风重写",
  review: "章节审校",
  polish: "选段修文",
  continue: "光标续写",
  rewrite: "重写本章",
  extract: "资料整理",
  test: "连通测试",
};

function fmtTokens(v) {
  if (v === null || v === undefined) return "—";
  return String(v);
}

async function openUsage() {
  const u = await api(`/api/novels/${state.novelId}/usage`);
  const t = u.totals || {};
  const hitRate =
    t.input && t.cache_hit !== null && t.cache_hit !== undefined
      ? `（${Math.round((t.cache_hit / t.input) * 100)}%）`
      : "";
  $("#usage-stats").innerHTML = [
    `<span class="chip">调用 ${t.calls || 0} 次</span>`,
    `<span class="chip">输入 ${fmtTokens(t.input)}</span>`,
    `<span class="chip ${t.cache_hit ? "chip-success" : ""}">缓存命中 ${fmtTokens(t.cache_hit)}${hitRate}</span>`,
    `<span class="chip">未命中 ${fmtTokens(t.cache_miss)}</span>`,
    `<span class="chip">输出 ${fmtTokens(t.output)}</span>`,
    `<span class="chip chip-dark">合计 ${fmtTokens(t.total)}</span>`,
  ].join("");
  const row = (key, g) => `
    <tr>
      <td>${esc(key)}</td>
      <td class="num">${g.calls}</td>
      <td class="num">${fmtTokens(g.input)}</td>
      <td class="num">${fmtTokens(g.cache_hit)}</td>
      <td class="num">${fmtTokens(g.cache_miss)}</td>
      <td class="num">${fmtTokens(g.output)}</td>
      <td class="num">${fmtTokens(g.total)}</td>
    </tr>`;
  const empty = `<tr><td colspan="7" class="muted">暂无记录</td></tr>`;
  $("#usage-task-rows").innerHTML =
    (u.by_task || []).map((g) => row(USAGE_TASK_LABEL[g.key] || g.key, g)).join("") || empty;
  $("#usage-chapter-rows").innerHTML =
    (u.by_chapter || []).map((g) => row(g.key, g)).join("") || empty;
  $("#usage-backdrop").hidden = false;
}

function renderChapters() {
  const chs = (state.novel?.chapters || []);
  const tbody = $("#chapter-rows");
  tbody.innerHTML = chs.length
    ? chs.map((c) => `
      <tr>
        <td class="num">${c.idx}</td>
        <td>${esc(c.title || `第${c.idx}章`)}</td>
        <td>${c.status === "done" ? `<span class="chip chip-success">已完成</span>` : c.status === "drafted" ? `<span class="chip chip-info">待审校</span>` : `<span class="chip">已规划</span>`}</td>
        <td class="num">${c.words || 0}</td>
        <td>${reviewChip(c.review)}</td>
        <td class="row-actions">
          ${c.status === "planned"
            ? `<button class="btn btn-outline btn-sm" data-plan="${c.idx}">改规划</button>`
            : `<button class="icon-btn" data-read="${c.idx}" title="阅读 / 编辑">${ic("i-eye")}</button>`}
          <button class="icon-btn" data-move="${c.idx}" data-dir="up" title="上移">${ic("i-up")}</button>
          <button class="icon-btn" data-move="${c.idx}" data-dir="down" title="下移">${ic("i-down")}</button>
          <button class="icon-btn danger" data-del="${c.idx}" title="删除本章">${ic("i-x")}</button>
        </td>
      </tr>`).join("")
    : `<tr><td colspan="6" class="muted">章节规划生成后会显示在这里。</td></tr>`;
}

async function renderCharacters() {
  const data = await api(`/api/novels/${state.novelId}/characters`);
  const items = data.items || [];
  $("#character-rows").innerHTML = items.length
    ? items.map((c) => `
      <tr>
        <td>${esc(c.name)}</td>
        <td>${c.role ? `<span class="chip">${esc(c.role)}</span>` : "—"}</td>
        <td><span class="chip ${c.status === "dead" ? "chip-danger" : c.status === "leave" ? "chip-warning" : "chip-success"}">${esc(CHARACTER_STATUS[c.status] || c.status)}</span></td>
        <td class="facts-cell">${c.facts.length ? esc(c.facts.slice(-3).join("；")) : "—"}</td>
        <td>
          <button class="btn btn-outline btn-sm" data-char-edit="${c.id}">编辑</button>
          <button class="btn btn-ghost btn-sm btn-danger" data-char-del="${c.id}">删除</button>
        </td>
      </tr>`).join("")
    : `<tr><td colspan="5" class="muted">写完章节后会自动整理角色；也可以点「新增角色」手动维护。</td></tr>`;
}

async function renderForeshadowings() {
  const data = await api(`/api/novels/${state.novelId}/foreshadowings`);
  const items = data.items || [];
  $("#foreshadow-rows").innerHTML = items.length
    ? items.map((f) => `
      <tr>
        <td>${esc(f.title)}</td>
        <td><span class="chip ${f.status === "open" ? "chip-warning" : "chip-success"}">${f.status === "open" ? "未回收" : "已回收"}</span></td>
        <td class="num">${f.planted_chapter > 0 ? `第${f.planted_chapter}章` : "手动"}</td>
        <td class="num">${f.resolved_chapter ? `第${f.resolved_chapter}章` : "—"}</td>
        <td class="facts-cell">${f.notes ? esc(f.notes) : "—"}</td>
        <td>
          ${f.status === "open" ? `<button class="btn btn-outline btn-sm" data-fore-resolve="${f.id}" data-title="${esc(f.title)}">标记回收</button>` : ""}
          <button class="btn btn-outline btn-sm" data-fore-edit="${f.id}">编辑</button>
          <button class="btn btn-ghost btn-sm btn-danger" data-fore-del="${f.id}">删除</button>
        </td>
      </tr>`).join("")
    : `<tr><td colspan="6" class="muted">写后会自动整理伏笔（埋下/回收）；也可以点「新增伏笔」手动记录。</td></tr>`;
}

function bindNovel() {
  $("#chapter-rows").addEventListener("click", async (e) => {
    const read = e.target.closest("[data-read]");
    if (read) { openEditor(parseInt(read.dataset.read, 10)).catch((err) => toast(err.message, true)); return; }
    const plan = e.target.closest("[data-plan]");
    if (plan) { editPlan(parseInt(plan.dataset.plan, 10)).catch((err) => toast(err.message, true)); return; }
    const move = e.target.closest("[data-move]");
    if (move) {
      try {
        await apiSend(`/api/novels/${state.novelId}/chapters/${move.dataset.move}/move`, "PUT", { direction: move.dataset.dir });
        await refreshNovel();
      } catch (err) { toast(err.message, true); }
      return;
    }
    const del = e.target.closest("[data-del]");
    if (del) {
      if (!confirm(`删除第 ${del.dataset.del} 章？后续章节会自动重排，且不可恢复。`)) return;
      try {
        await api(`/api/novels/${state.novelId}/chapters/${del.dataset.del}`, { method: "DELETE" });
        toast("已删除并重排");
        await refreshNovel();
      } catch (err) { toast(err.message, true); }
    }
  });
  $("#character-rows").addEventListener("click", (e) => {
    const edit = e.target.closest("[data-char-edit]");
    if (edit) { editCharacter(edit.dataset.charEdit).catch((err) => toast(err.message, true)); return; }
    const del = e.target.closest("[data-char-del]");
    if (del) {
      if (!confirm("删除该角色及其全部事实记录？")) return;
      api(`/api/novels/${state.novelId}/characters/${del.dataset.charDel}`, { method: "DELETE" })
        .then(() => { toast("已删除"); return renderCharacters(); })
        .catch((err) => toast(err.message, true));
    }
  });
  $("#foreshadow-rows").addEventListener("click", async (e) => {
    const resolve = e.target.closest("[data-fore-resolve]");
    if (resolve) {
      try {
        await apiSend(`/api/novels/${state.novelId}/foreshadowings/${resolve.dataset.foreResolve}`, "PUT", {
          status: "resolved",
          notes: "",
          resolved_chapter: (state.novel?.chapters || []).length || null,
        });
        toast("已标记回收");
        await renderForeshadowings();
      } catch (err) { toast(err.message, true); }
      return;
    }
    const edit = e.target.closest("[data-fore-edit]");
    if (edit) { editForeshadowing(edit.dataset.foreEdit).catch((err) => toast(err.message, true)); return; }
    const del = e.target.closest("[data-fore-del]");
    if (del) {
      if (!confirm("删除该伏笔记录？")) return;
      api(`/api/novels/${state.novelId}/foreshadowings/${del.dataset.foreDel}`, { method: "DELETE" })
        .then(() => { toast("已删除"); return renderForeshadowings(); })
        .catch((err) => toast(err.message, true));
    }
  });
  $("#novel-tabs").addEventListener("click", (e) => {
    const chip = e.target.closest("[data-tab]");
    if (!chip) return;
    state.tab = chip.dataset.tab;
    renderNovel();
  });
  $("#mode-toggle").addEventListener("click", async (e) => {
    const chip = e.target.closest("[data-mode]");
    if (!chip) return;
    try {
      await apiSend(`/api/novels/${state.novelId}`, "PATCH", { mode: chip.dataset.mode });
      toast(chip.dataset.mode === "staged" ? "已切换：规划后会暂停等你确认" : "已切换：全自动推进");
      await refreshNovel();
    } catch (err) { toast(err.message, true); }
  });
  $("#add-character").onclick = () => {
    openForm({
      title: "新增角色",
      fields: [
        { name: "name", label: "角色名", placeholder: "如：林深" },
        { name: "role", label: "定位", placeholder: "主角 / 配角 / 反派…" },
      ],
      onSubmit: async (v) => {
        if (!v.name.trim()) throw new Error("角色名不能为空");
        await apiSend(`/api/novels/${state.novelId}/characters`, "POST", { name: v.name, role: v.role });
        toast("已新增角色");
        await renderCharacters();
      },
    });
  };
  $("#add-foreshadowing").onclick = () => {
    openForm({
      title: "新增伏笔",
      fields: [
        { name: "title", label: "伏笔内容", placeholder: "一句话概括，如：姐姐留下的鲸歌录音" },
        { name: "notes", label: "备注", type: "textarea", rows: 3, placeholder: "计划在第几章回收、当前线索…" },
      ],
      onSubmit: async (v) => {
        if (!v.title.trim()) throw new Error("伏笔内容不能为空");
        await apiSend(`/api/novels/${state.novelId}/foreshadowings`, "POST", { title: v.title, notes: v.notes });
        toast("已新增伏笔");
        await renderForeshadowings();
      },
    });
  };
  $("#novel-insights").addEventListener("click", (e) => {
    const chip = e.target.closest("[data-insight]");
    if (!chip) return;
    state.tab = chip.dataset.insight;
    renderNovel();
  });
  $("#novel-chips").addEventListener("click", (e) => {
    if (e.target.closest("#novel-style-chip")) openStylePicker();
  });
  $("#style-close").onclick = () => { $("#style-backdrop").hidden = true; };
  $("#insert-chapter").onclick = () => {
    const count = (state.novel?.chapters || []).length;
    openForm({
      title: "插入章节",
      fields: [
        { name: "after_idx", label: "插入位置（在第 N 章之后，0 = 最前）", value: String(count) },
        { name: "title", label: "章节标题", value: "新章节" },
        { name: "goal", label: "本章目标（可选）" },
      ],
      onSubmit: async (v) => {
        const after = Math.max(0, parseInt(v.after_idx || "0", 10) || 0);
        await apiSend(`/api/novels/${state.novelId}/chapters`, "POST", {
          after_idx: after,
          title: v.title,
          goal: v.goal,
        });
        toast("已插入章节");
        await refreshNovel();
      },
    });
  };
  $("#novel-target").addEventListener("input", (e) => { e.target.dataset.touched = "1"; });
  $("#novel-run").onclick = async () => {
    const chapters = Math.min(50, Math.max(1, parseInt($("#novel-target").value || "3", 10) || 3));
    try {
      await apiSend(`/api/novels/${state.novelId}/run`, "POST", { chapters });
      toast(`已开始，目标 ${chapters} 章（已完成章节会自动跳过）`);
      $("#novel-run").disabled = true;
      $("#novel-run").textContent = "创作中…";
      $("#novel-run").classList.remove("attention");
    } catch (err) {
      toast(err.message, true);
      refreshNovel();
    }
  };
  $("#novel-delete").onclick = async () => {
    const title = $("#novel-title").textContent;
    if (!confirm(`确定删除《${title}》？全部章节、台账与运行记录都会被删除。`)) return;
    try {
      await api(`/api/novels/${state.novelId}`, { method: "DELETE" });
      toast("已删除");
      location.hash = "#/shelf";
    } catch (err) {
      toast(err.message, true);
    }
  };
  $("#novel-health").onclick = () => openHealth().catch((err) => toast(err.message, true));
  $("#health-close").onclick = () => { $("#health-backdrop").hidden = true; };
  $("#health-fill").onclick = () => fillMissing().catch((err) => toast(err.message, true));
  $("#novel-usage").onclick = () => openUsage().catch((err) => toast(err.message, true));
  $("#usage-close").onclick = () => { $("#usage-backdrop").hidden = true; };
  $("#stream-clear").onclick = () => {
    state.stream = { chapter: null, text: "" };
    const pre = $("#stream-pre");
    pre.textContent = "等待生成…";
    pre.classList.add("muted");
    $("#stream-live").classList.remove("on");
  };
}

async function editPlan(idx) {
  const c = (state.novel?.chapters || []).find((x) => x.idx === idx) || {};
  openForm({
    title: `编辑第 ${idx} 章规划`,
    fields: [
      { name: "title", label: "章节标题", value: c.title || "" },
      { name: "goal", label: "本章目标", value: c.goal || "", placeholder: "30 字以内" },
      { name: "beats", label: "关键节拍（每行一条）", type: "textarea", rows: 4, value: (c.beats || []).join("\n") },
    ],
    onSubmit: async (v) => {
      if (!v.title.trim()) throw new Error("标题不能为空");
      const beats = v.beats.split("\n").map((s) => s.trim()).filter(Boolean);
      await apiSend(`/api/novels/${state.novelId}/chapters/${idx}/plan`, "PUT", {
        title: v.title,
        goal: v.goal,
        beats,
      });
      toast("规划已更新");
      await refreshNovel();
    },
  });
}

async function editCharacter(id) {
  const data = await api(`/api/novels/${state.novelId}/characters`);
  const c = (data.items || []).find((x) => x.id === id);
  if (!c) throw new Error("角色不存在");
  openForm({
    title: `编辑角色：${c.name}`,
    fields: [
      { name: "role", label: "定位", value: c.role || "" },
      { name: "status", label: "状态", type: "select", value: c.status, options: [
        { value: "active", label: "在场" }, { value: "dead", label: "死亡" }, { value: "leave", label: "离场" },
      ] },
      { name: "facts", label: "关键事实（每行一条）", type: "textarea", rows: 6, value: (c.facts || []).join("\n") },
    ],
    onSubmit: async (v) => {
      await apiSend(`/api/novels/${state.novelId}/characters/${id}`, "PUT", {
        role: v.role,
        status: v.status,
        facts: v.facts.split("\n").map((s) => s.trim()).filter(Boolean),
      });
      toast("角色已更新");
      await renderCharacters();
    },
  });
}

async function editForeshadowing(id) {
  const data = await api(`/api/novels/${state.novelId}/foreshadowings`);
  const f = (data.items || []).find((x) => x.id === id);
  if (!f) throw new Error("伏笔不存在");
  openForm({
    title: "编辑伏笔",
    fields: [
      { name: "status", label: "状态", type: "select", value: f.status, options: [
        { value: "open", label: "未回收" }, { value: "resolved", label: "已回收" },
      ] },
      { name: "notes", label: "备注", type: "textarea", rows: 4, value: f.notes || "" },
    ],
    onSubmit: async (v) => {
      await apiSend(`/api/novels/${state.novelId}/foreshadowings/${id}`, "PUT", {
        status: v.status,
        notes: v.notes,
        resolved_chapter: v.status === "resolved" ? (f.resolved_chapter || (state.novel?.chapters || []).length || null) : null,
      });
      toast("伏笔已更新");
      await renderForeshadowings();
    },
  });
}

/* ---------------- 章节编辑器 ---------------- */

async function openEditor(idx) {
  const c = await api(`/api/novels/${state.novelId}/chapters/${idx}`);
  $("#reader-title").textContent = `第${c.idx}章 ${c.title || ""}`;
  renderReviewChips(c.review);
  renderStyleReport(c.style_report);
  $("#reader-goal").textContent = [
    c.goal ? `本章目标：${c.goal}` : "",
    c.words ? `字数：${c.words}` : "",
  ].filter(Boolean).join(" · ");
  $("#reader-summary").textContent = c.summary ? `本章摘要：${c.summary}` : "";

  const ta = $("#reader-content");
  ta.value = c.content || "";
  ta.dataset.idx = String(idx);

  // 防丢稿：恢复本机草稿
  try {
    const raw = localStorage.getItem(draftKey(idx));
    if (raw) {
      const d = JSON.parse(raw);
      if (d && typeof d.content === "string" && d.content !== (c.content || "")) {
        const when = new Date(d.at || Date.now()).toLocaleString();
        if (confirm(`发现 ${when} 的未保存草稿，恢复它吗？（取消则丢弃该草稿）`)) {
          ta.value = d.content;
        } else {
          localStorage.removeItem(draftKey(idx));
        }
      }
    }
  } catch { /* 忽略 */ }

  state.editorDirty = false;
  $("#reader-save").disabled = true;
  $("#reader-backdrop").hidden = false;
}

function renderReviewChips(raw) {
  const review = parseReview(raw);
  if (!review) {
    $("#reader-review").innerHTML = "";
    return;
  }
  state.editorIssues = review.issues;
  $("#reader-review").innerHTML = review.issues.length
    ? review.issues
        .map((it, i) => {
          const cls = it.severity === "high" ? "chip-danger" : it.severity === "medium" ? "chip-warning" : "";
          return `<span class="chip chip-filter ${cls}" data-fix="${i}" title="点击：按这条意见修文">${ic("i-flag")}${esc(it.note || it.severity)}</span>`;
        })
        .join("")
    : `<span class="chip chip-success">审校通过</span>`;
}

/** 本地文风检查结果（零 token）：每项规则 ✓/✗ + 细节 */
function renderStyleReport(report) {
  const el = $("#reader-style");
  if (!el) return;
  if (!report || !Array.isArray(report.checks) || !report.checks.length) {
    el.hidden = true;
    el.innerHTML = "";
    return;
  }
  const styleName = (() => {
    const s = styleById(report.style);
    return s ? s.name : report.style;
  })();
  el.hidden = false;
  const failed = report.checks.filter((c) => !c.passed);
  const detailEl = $("#reader-style-detail");
  if (detailEl) {
    detailEl.textContent = failed.length
      ? failed.map((c) => `${c.name}：${c.detail}`).join("；")
      : "";
    detailEl.hidden = true;
  }
  el.innerHTML =
    `<span class="chip chip-dark">文风检查 · ${esc(styleName)}</span>` +
    report.checks
      .map((c) => {
        const cls = c.passed ? "chip-success" : c.hard ? "chip-danger" : "chip-warning";
        return `<span class="chip ${cls}" title="${esc(c.detail)}">${c.passed ? "✓" : "✗"} ${esc(c.name)}</span>`;
      })
      .join("") +
    (failed.length ? `<span class="chip chip-filter" data-styledetail="1" title="点击查看细节">细节</span>` : "");
}

function draftKey(idx) {
  return `ainovel.draft.${state.novelId}.${idx}`;
}

function closeReader() {
  $("#reader-backdrop").hidden = true;
}

let draftTimer = null;

async function saveChapter(silent = false) {
  const ta = $("#reader-content");
  const idx = parseInt(ta.dataset.idx || "0", 10);
  if (!idx) return false;
  try {
    await apiSend(`/api/novels/${state.novelId}/chapters/${idx}`, "PUT", { content: ta.value });
    state.editorDirty = false;
    $("#reader-save").disabled = true;
    try { localStorage.removeItem(draftKey(idx)); } catch { /* 忽略 */ }
    if (!silent) toast("已保存");
    await refreshNovel();
    return true;
  } catch (err) {
    toast(err.message, true);
    return false;
  }
}

function bindEditor() {
  const ta = $("#reader-content");
  ta.addEventListener("input", () => {
    state.editorDirty = true;
    $("#reader-save").disabled = false;
    // 防丢稿：本地草稿（700ms 防抖）
    clearTimeout(draftTimer);
    draftTimer = setTimeout(() => {
      const idx = parseInt(ta.dataset.idx || "0", 10);
      if (!idx) return;
      try {
        localStorage.setItem(draftKey(idx), JSON.stringify({ content: ta.value, at: Date.now() }));
      } catch { /* 忽略 */ }
    }, 700);
  });
  $("#reader-close").onclick = () => {
    if (state.editorDirty && !confirm("有未保存的修改，确定关闭？")) return;
    closeReader();
  };
  $("#reader-backdrop").addEventListener("click", (e) => {
    if (e.target === $("#reader-backdrop")) {
      if (state.editorDirty && !confirm("有未保存的修改，确定关闭？")) return;
      closeReader();
    }
  });
  $("#reader-save").onclick = () => { saveChapter(); };
  $("#reader-recheck").onclick = async () => {
    if (state.editorDirty) {
      if (!confirm("先保存当前修改，再重新审校？")) return;
      if (!(await saveChapter(true))) return;
    }
    const idx = parseInt(ta.dataset.idx || "0", 10);
    if (!idx) return;
    toast("重新审校中…");
    try {
      const r = await api(`/api/novels/${state.novelId}/chapters/${idx}/review`, { method: "POST" });
      renderReviewChips(JSON.stringify(r.review));
      toast(r.review.ok ? "复核完成：通过" : `复核完成：${r.review.issues.length} 条提醒`);
      await refreshNovel();
    } catch (err) {
      toast(err.message, true);
    }
  };
  $("#reader-review").addEventListener("click", (e) => {
    const chip = e.target.closest("[data-fix]");
    if (!chip) return;
    const issue = (state.editorIssues || [])[parseInt(chip.dataset.fix, 10)];
    if (issue) fixIssue(issue).catch((err) => toast(err.message, true));
  });
  $("#reader-style").addEventListener("click", (e) => {
    if (!e.target.closest("[data-styledetail]")) return;
    const d = $("#reader-style-detail");
    if (d) d.hidden = !d.hidden;
  });
  $$("[data-polish]").forEach((btn) => {
    btn.onclick = () => runPolish(btn.dataset.polish).catch((err) => toast(err.message, true));
  });
  $("#diff-cancel").onclick = closeDiff;
  $("#diff-apply").onclick = applyPolish;
  $("#diff-reroll").onclick = () => rerollPolish().catch((err) => toast(err.message, true));
  $("#reader-continue").onclick = () => continueFlow();
  $("#reader-rewrite").onclick = () => rewriteFlow();
  $("#gen-cancel").onclick = closeGen;
  $("#gen-apply").onclick = applyGen;
  $("#gen-retry").onclick = () => retryGen().catch((err) => toast(err.message, true));
}

async function runPolish(mode) {
  const ta = $("#reader-content");
  const start = ta.selectionStart;
  const end = ta.selectionEnd;
  const text = ta.value.slice(start, end).trim();
  if (text.length < 8) {
    toast("请先在正文中选中至少 8 个字", true);
    return;
  }
  const idx = parseInt(ta.dataset.idx || "0", 10);
  toast("AI 修文中…");
  const r = await apiSend(`/api/novels/${state.novelId}/chapters/${idx}/polish`, "POST", {
    mode,
    selection: text,
    instruction: $("#polish-instruction").value.trim(),
  });
  const revised = r.revised || "";
  if (!revised || revised === text) {
    toast("没有产生修改，请调整选中内容或换个模式", true);
    return;
  }
  state.polishRange = { start, end, text };
  state.polishResult = revised;
  state.polishLast = { mode, selection: text, instruction: $("#polish-instruction").value.trim() };
  $("#diff-feedback").value = "";
  showDiff(mode, text, revised);
}

const POLISH_LABELS = { rewrite: "改写", expand: "扩写", shrink: "精简", depolish: "去 AI 味" };

function showDiff(mode, original, revised) {
  $("#diff-mode").textContent = POLISH_LABELS[mode] || mode;
  const d = diffParts(original, revised);
  $("#diff-original").innerHTML = `${esc(d.pre)}<mark class="del">${esc(d.aMid)}</mark>${esc(d.suf)}`;
  $("#diff-revised").innerHTML = `${esc(d.pre)}<mark class="add">${esc(d.bMid)}</mark>${esc(d.suf)}`;
  $("#diff-backdrop").hidden = false;
}

function closeDiff() {
  $("#diff-backdrop").hidden = true;
  state.polishResult = null;
  state.polishRange = null;
  const fb = $("#diff-feedback");
  if (fb) fb.value = "";
}

function applyPolish() {
  const ta = $("#reader-content");
  const range = state.polishRange;
  const revised = state.polishResult;
  closeDiff();
  if (!range || !revised) return;
  // 若编辑器内容已被改动且选中区间不再匹配原文，则退回“追加提示”
  if (ta.value.slice(range.start, range.end) !== range.text) {
    toast("正文已被改动，请重新选中后再应用", true);
    return;
  }
  ta.setRangeText(revised, range.start, range.end, "end");
  state.editorDirty = true;
  $("#reader-save").disabled = false;
  toast("已应用修改，记得点「保存修改」");
}

/** 按一条审校意见直接修文（优先定位意见引用的原文片段） */
async function fixIssue(issue) {
  const ta = $("#reader-content");
  let start = ta.selectionStart;
  let end = ta.selectionEnd;
  let text = ta.value.slice(start, end).trim();
  if (text.length < 8 && issue.quote) {
    const pos = ta.value.indexOf(issue.quote);
    if (pos >= 0) {
      start = pos;
      end = pos + issue.quote.length;
      text = issue.quote;
      ta.focus();
      ta.setSelectionRange(start, end);
    }
  }
  if (text.length < 8) {
    toast("没有定位到对应原文，请手动选中要修的段落", true);
    return;
  }
  const idx = parseInt(ta.dataset.idx || "0", 10);
  toast("按审校意见修文中…");
  const r = await apiSend(`/api/novels/${state.novelId}/chapters/${idx}/polish`, "POST", {
    mode: "rewrite",
    selection: text,
    instruction: issue.note || "",
  });
  const revised = r.revised || "";
  if (!revised || revised === text) {
    toast("没有产生修改，请手动选中相关段落再试", true);
    return;
  }
  state.polishRange = { start, end, text };
  state.polishResult = revised;
  state.polishLast = { mode: "rewrite", selection: text, instruction: issue.note || "" };
  $("#diff-feedback").value = "";
  showDiff("rewrite", text, revised);
}

/** 修文可迭代：按「修改要求」再改一版（始终基于最初的选中原文，避免层层漂移） */
async function rerollPolish() {
  const last = state.polishLast;
  if (!last) return;
  const feedback = $("#diff-feedback").value.trim();
  const instruction = [last.instruction, feedback].filter(Boolean).join("；");
  toast("再改一版中…");
  const idx = parseInt($("#reader-content").dataset.idx || "0", 10);
  const r = await apiSend(`/api/novels/${state.novelId}/chapters/${idx}/polish`, "POST", {
    mode: last.mode,
    selection: last.selection,
    instruction,
  });
  if (!r.revised) return;
  state.polishResult = r.revised;
  showDiff(last.mode, last.selection, r.revised);
}

/* ---------------- 写作工具：光标续写 / 重写本章 ---------------- */

function continueFlow() {
  const ta = $("#reader-content");
  const pos = ta.selectionStart;
  const before = ta.value.slice(0, pos);
  if (before.trim().length < 4) {
    toast("请先把光标放到要续写的位置", true);
    return;
  }
  openForm({
    title: "光标续写",
    fields: [
      { name: "chars", label: "续写字数（100–2000）", value: "600" },
      { name: "instruction", label: "续写要求（可选）", placeholder: "如：加一段对话 / 收束场景" },
    ],
    onSubmit: async (v) => {
      const chars = Math.min(2000, Math.max(100, parseInt(v.chars || "600", 10) || 600));
      toast("续写中…");
      const payload = { before, chars, instruction: (v.instruction || "").trim() };
      const r = await apiSend(`/api/novels/${state.novelId}/chapters/${ta.dataset.idx}/continue`, "POST", payload);
      state.gen = { kind: "continue", payload, cursorPos: pos, text: r.text };
      showGen("光标续写", `${chars} 字`, r.text);
    },
  });
}

function rewriteFlow() {
  const ta = $("#reader-content");
  const idx = ta.dataset.idx;
  if (!idx) return;
  const hasContent = ta.value.trim().length > 0;
  openForm({
    title: "重写本章",
    fields: [
      {
        name: "mode",
        label: "重写方式",
        type: "select",
        value: hasContent ? "revise" : "regenerate",
        options: [
          { value: "revise", label: "按意见改写现有正文" },
          { value: "regenerate", label: "按章纲重新生成整章" },
        ],
      },
      { name: "instruction", label: "改写要求（可选）", placeholder: "如：节奏再快一点 / 强化悬念" },
    ],
    onSubmit: async (v) => {
      toast("重写中…");
      const payload = { mode: v.mode, instruction: (v.instruction || "").trim() };
      const r = await apiSend(`/api/novels/${state.novelId}/chapters/${idx}/rewrite`, "POST", payload);
      state.gen = { kind: "rewrite", payload: { ...payload, mode: r.mode }, text: r.text };
      showGen("重写结果", r.mode === "regenerate" ? "按章纲重新生成" : "按意见改写", r.text);
    },
  });
}

function showGen(title, badge, text) {
  $("#gen-title").textContent = title;
  $("#gen-mode").textContent = badge;
  $("#gen-text").textContent = text;
  $("#gen-backdrop").hidden = false;
}

function closeGen() {
  $("#gen-backdrop").hidden = true;
}

function applyGen() {
  const gen = state.gen;
  if (!gen) return;
  const ta = $("#reader-content");
  if (gen.kind === "continue") {
    const pos = Math.min(gen.cursorPos ?? ta.selectionStart, ta.value.length);
    ta.setRangeText(gen.text, pos, pos, "end");
  } else {
    ta.value = gen.text;
    ta.setSelectionRange(0, 0);
  }
  state.editorDirty = true;
  $("#reader-save").disabled = false;
  closeGen();
  toast("已应用到正文，记得点「保存修改」");
}

async function retryGen() {
  const gen = state.gen;
  if (!gen) return;
  toast("再来一版中…");
  const ta = $("#reader-content");
  const path = gen.kind === "continue" ? "continue" : "rewrite";
  const r = await apiSend(`/api/novels/${state.novelId}/chapters/${ta.dataset.idx}/${path}`, "POST", gen.payload);
  state.gen = { ...gen, text: r.text };
  $("#gen-text").textContent = r.text;
}

/** 极简 diff：公共前缀/后缀定位变动区，中间高亮 */
function diffParts(a, b) {
  const maxPre = Math.min(a.length, b.length);
  let pre = 0;
  while (pre < maxPre && a[pre] === b[pre]) pre++;
  let suf = 0;
  while (suf < maxPre - pre && a[a.length - 1 - suf] === b[b.length - 1 - suf]) suf++;
  return {
    pre: a.slice(0, pre),
    aMid: a.slice(pre, a.length - suf),
    bMid: b.slice(pre, b.length - suf),
    suf: a.slice(a.length - suf),
  };
}

/* ---------------- 通用表单弹层 ---------------- */

let formSubmit = null;

function openForm({ title, fields, onSubmit }) {
  $("#form-title").textContent = title;
  $("#form-fields").innerHTML = fields
    .map((f) => {
      if (f.type === "textarea") {
        return `<div class="field"><label>${esc(f.label)}</label><textarea data-field="${esc(f.name)}" rows="${f.rows || 4}" placeholder="${esc(f.placeholder || "")}">${esc(f.value || "")}</textarea></div>`;
      }
      if (f.type === "select") {
        return `<div class="field"><label>${esc(f.label)}</label><select data-field="${esc(f.name)}">${(f.options || [])
          .map((o) => `<option value="${esc(o.value)}" ${o.value === f.value ? "selected" : ""}>${esc(o.label)}</option>`)
          .join("")}</select></div>`;
      }
      return `<div class="field"><label>${esc(f.label)}</label><input data-field="${esc(f.name)}" value="${esc(f.value || "")}" placeholder="${esc(f.placeholder || "")}" /></div>`;
    })
    .join("");
  formSubmit = onSubmit;
  $("#form-backdrop").hidden = false;
}

function closeForm() {
  $("#form-backdrop").hidden = true;
  formSubmit = null;
}

function bindFormDialog() {
  const submit = async () => {
    if (!formSubmit) return;
    const values = {};
    $$("#form-fields [data-field]").forEach((el) => { values[el.dataset.field] = el.value; });
    try {
      await formSubmit(values);
      closeForm();
    } catch (err) {
      toast(err.message, true);
    }
  };
  $("#form-save").onclick = submit;
  $("#form-close").onclick = closeForm;
  $("#form-cancel").onclick = closeForm;
  $("#form-backdrop").addEventListener("click", (e) => {
    if (e.target === $("#form-backdrop")) closeForm();
  });
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
  const es = new EventSource(withToken(`/api/novels/${id}/events`));
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
    case "ledger_updated":
      novelLog(`资料整理：角色 ${ev.characters} 项 / 伏笔 ${ev.foreshadowings} 项`);
      if (state.tab !== "chapters") renderTab();
      break;
    case "run_paused":
      novelLog(ev.message);
      toast("已暂停：章节规划待确认");
      $("#stream-live").classList.remove("on");
      refreshNovel();
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
        <td>${r.status === "done" ? `<span class="chip chip-success">已完成</span>` : r.status === "failed" ? `<span class="chip chip-danger">失败</span>` : r.status === "paused" ? `<span class="chip chip-warning">待确认</span>` : `<span class="chip chip-info">执行中</span>`}</td>
        <td>${esc(STAGE_LABEL[r.stage] || r.stage)}</td>
        <td class="muted">${r.error ? esc(r.error) : "—"}</td>
      </tr>`;
    }).join("")
    : `<tr><td colspan="7" class="muted">还没有运行记录。</td></tr>`;
}

/* ---------------- 提示词模板 ---------------- */

let promptsCache = null;
let promptFocusTarget = null;

async function loadPrompts(force = false) {
  if (!promptsCache || force) {
    const r = await api("/api/prompts");
    promptsCache = r.items || [];
  }
  return promptsCache;
}

async function renderPromptList() {
  const list = await loadPrompts(true);
  const el = $("#prompt-list");
  if (!el) return;
  el.innerHTML = list.map((t) => `
    <div class="prompt-row">
      <div class="prompt-row-main">
        <div class="prompt-row-title">${esc(t.label)} <span class="prompt-id">${esc(t.id)}</span></div>
        <div class="prompt-row-desc">${esc(t.desc)}</div>
      </div>
      <div class="prompt-row-side">
        <span class="chip ${t.custom ? "chip-info" : ""}">${t.custom ? "已自定义" : "默认"}</span>
        <button class="btn btn-outline btn-sm" data-pedit="${esc(t.id)}">编辑</button>
      </div>
    </div>`).join("");
}

function insertAtCursor(ta, text) {
  const start = typeof ta.selectionStart === "number" ? ta.selectionStart : ta.value.length;
  const end = typeof ta.selectionEnd === "number" ? ta.selectionEnd : start;
  ta.value = ta.value.slice(0, start) + text + ta.value.slice(end);
  ta.focus();
  ta.selectionStart = ta.selectionEnd = start + text.length;
}

async function openPromptEditor(id) {
  const list = await loadPrompts();
  const t = list.find((x) => x.id === id);
  if (!t) return;
  state.promptTask = t;
  $("#prompt-title").textContent = `编辑提示词 · ${t.label}`;
  $("#prompt-desc").textContent = t.desc;
  $("#prompt-state").textContent = t.custom ? "当前：已自定义" : "当前：默认模板";
  $("#prompt-error").textContent = "";
  const src = t.custom || { system: t.default_system, user: t.default_user };
  $("#prompt-system").value = src.system;
  $("#prompt-user").value = src.user;
  $("#prompt-vars").innerHTML = t.vars
    .map((v) => `<span class="chip chip-filter" data-pvar="{{${esc(v.name)}}}" title="${esc(v.desc)}｜示例：${esc(v.sample)}">{{${esc(v.name)}}}</span>`)
    .join("");
  promptFocusTarget = $("#prompt-user");
  $("#prompt-backdrop").hidden = false;
}

function bindPrompts() {
  $("#prompt-close").onclick = () => { $("#prompt-backdrop").hidden = true; };
  $("#prompt-list").addEventListener("click", (e) => {
    const b = e.target.closest("[data-pedit]");
    if (!b) return;
    openPromptEditor(b.dataset.pedit).catch((err) => toast(err.message, true));
  });
  ["#prompt-system", "#prompt-user"].forEach((sel) => {
    $(sel).addEventListener("focusin", () => { promptFocusTarget = $(sel); });
  });
  $("#prompt-vars").addEventListener("click", (e) => {
    const c = e.target.closest("[data-pvar]");
    if (!c) return;
    const ta = promptFocusTarget && document.contains(promptFocusTarget) ? promptFocusTarget : $("#prompt-user");
    insertAtCursor(ta, c.dataset.pvar);
  });
  $("#prompt-save").onclick = async () => {
    const t = state.promptTask;
    if (!t) return;
    $("#prompt-error").textContent = "";
    try {
      await apiSend(`/api/prompts/${t.id}`, "PUT", {
        system: $("#prompt-system").value,
        user: $("#prompt-user").value,
      });
      toast(`已保存：${t.label}（对后续生成生效）`);
      $("#prompt-backdrop").hidden = true;
      await renderPromptList();
    } catch (err) {
      $("#prompt-error").textContent = err.message;
    }
  };
  $("#prompt-reset").onclick = async () => {
    const t = state.promptTask;
    if (!t) return;
    try {
      await api(`/api/prompts/${t.id}`, { method: "DELETE" });
      toast(`已恢复默认：${t.label}`);
      $("#prompt-backdrop").hidden = true;
      await renderPromptList();
    } catch (err) {
      $("#prompt-error").textContent = err.message;
    }
  };
  $("#prompt-load-default").onclick = () => {
    const t = state.promptTask;
    if (!t) return;
    $("#prompt-system").value = t.default_system;
    $("#prompt-user").value = t.default_user;
  };
}

/* ---------------- 设置 ---------------- */

const PRESETS = {
  // 国内
  deepseek: { base: "https://api.deepseek.com/v1", model: "deepseek-chat" },
  qwen: { base: "https://dashscope.aliyuncs.com/compatible-mode/v1", model: "qwen-plus" },
  glm: { base: "https://open.bigmodel.cn/api/paas/v4", model: "glm-4.5-air" },
  moonshot: { base: "https://api.moonshot.cn/v1", model: "kimi-latest" },
  siliconflow: { base: "https://api.siliconflow.cn/v1", model: "deepseek-ai/DeepSeek-V3" },
  // 国际
  openai: { base: "https://api.openai.com/v1", model: "gpt-4o-mini" },
  openrouter: { base: "https://openrouter.ai/api/v1", model: "openai/gpt-4o-mini" },
  xai: { base: "https://api.x.ai/v1", model: "grok-4" },
  groq: { base: "https://api.groq.com/openai/v1", model: "llama-3.3-70b-versatile" },
  opencode_zen: { base: "https://opencode.ai/zen/v1", model: "kimi-k2.5" },
  opencode_go: { base: "https://opencode.ai/inference/go/openai/v1", model: "kimi-k3" },
  commandcode: { base: "https://api.commandcode.ai/provider/v1", model: "deepseek/deepseek-v4-flash" },
  // 本地
  ollama: { base: "http://127.0.0.1:11434/v1", model: "qwen2.5:7b" },
  lmstudio: { base: "http://127.0.0.1:1234/v1", model: "qwen3-8b" },
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
  const backup = $("#set-backup");
  if (backup) backup.href = withToken("/api/backup");
  try {
    await renderPromptList();
  } catch (err) {
    const el = $("#prompt-status");
    if (el) el.textContent = "提示词目录加载失败：" + err.message;
  }
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
      await apiSend("/api/settings/llm", "PUT", body);
      state.settings = body;
      renderModelChip();
      $("#set-status").textContent = "已保存";
      setTimeout(() => ($("#set-status").textContent = ""), 2000);
    } catch (err) {
      toast(err.message, true);
    }
  });

  $("#set-provider").addEventListener("change", syncProviderFields);

  // 恢复备份：选文件 → 二次确认 → POST 原始 zip → 刷新全量数据
  $("#set-restore").onclick = () => {
    $("#set-backup-status").textContent = "";
    $("#set-restore-file").value = "";
    $("#set-restore-file").click();
  };
  $("#set-restore-file").addEventListener("change", async (e) => {
    const file = e.target.files && e.target.files[0];
    if (!file) return;
    if (!confirm(`用「${file.name}」恢复？当前全部作品与章节将被覆盖，且无法撤销。`)) return;
    const status = $("#set-backup-status");
    status.textContent = "恢复中…";
    try {
      const o = { method: "POST", headers: { "content-type": "application/zip" }, body: file };
      if (AUTH_TOKEN) o.headers["x-ainovel-token"] = AUTH_TOKEN;
      const r = await fetch("/api/restore", o);
      const text = await r.text();
      if (!r.ok) throw new Error(text || r.statusText);
      const info = JSON.parse(text);
      status.textContent = `已恢复 ${info.novels} 部作品`;
      toast("恢复完成");
      await loadSettings();
      location.hash = "#/shelf";
      route().catch((e2) => toast(e2.message, true));
    } catch (err) {
      status.textContent = "恢复失败：" + err.message;
      toast("恢复失败：" + err.message, true);
    }
  });

  $("#set-test").onclick = async () => {
    $("#set-status").textContent = "测试中…";
    try {
      const r = await api("/api/settings/llm/test", { method: "POST" });
      $("#set-status").textContent = r.ok ? `连接正常：${r.message}` : `连接失败：${r.message}`;
    } catch (err) {
      $("#set-status").textContent = "测试失败：" + err.message;
    }
  };

  const presetSel = $("#set-preset");
  if (presetSel) {
    presetSel.addEventListener("change", () => {
      const p = PRESETS[presetSel.value];
      if (!p) return;
      $("#set-provider").value = "openai";
      $("#set-base").value = p.base;
      $("#set-model").value = p.model;
      syncProviderFields();
      $("#set-status").textContent = "已填入预设，请补 API Key 后保存";
    });
  }

  // 拉取服务商模型列表（用表单里正在填的 Base URL / Key；为空回退到已保存设置）
  $("#set-models-fetch").onclick = async () => {
    const btn = $("#set-models-fetch");
    const sel = $("#set-model-list");
    btn.disabled = true;
    $("#set-status").textContent = "正在拉取模型列表…";
    try {
      const r = await apiSend("/api/settings/llm/models", "POST", {
        base_url: $("#set-base").value.trim(),
        api_key: $("#set-key").value.trim(),
      });
      if (!r.ok) throw new Error(r.message || "拉取失败");
      const models = r.models || [];
      sel.innerHTML = [`<option value="">选择模型（共 ${models.length} 个）…</option>`]
        .concat(models.map((m) => `<option value="${esc(m)}">${esc(m)}</option>`))
        .join("");
      sel.disabled = false;
      $("#set-status").textContent = `已拉取 ${models.length} 个模型，从下拉里选一个即可`;
    } catch (err) {
      sel.innerHTML = "<option>拉取失败</option>";
      sel.disabled = true;
      $("#set-status").textContent = "拉取失败：" + err.message;
    } finally {
      btn.disabled = false;
    }
  };
  $("#set-model-list").addEventListener("change", () => {
    const v = $("#set-model-list").value;
    if (v) {
      $("#set-model").value = v;
      $("#set-status").textContent = `已选择模型：${v}（记得保存）`;
    }
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
  bindEditor();
  bindFormDialog();
  bindSettings();
  bindPrompts();
  loadSettings().catch(() => {});
  loadHealth();
  setInterval(loadHealth, 15000);
  route().catch((e) => toast(e.message, true));
}

init();
