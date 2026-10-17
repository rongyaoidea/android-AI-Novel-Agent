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
  liveStage: null,
  stageSince: null,
  stageTimer: null,
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
  autofix: "自动修复",
  extract: "资料整理",
  auto: "自动连载",
  budget: "预算护栏",
  stop: "正在停止",
  stopped: "已停止",
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

/** 章节表里的审校列：与编辑器保持同一套状态（有未解决意见就如实显示「余留 N 条」，全部解决才显示「AI 已处理」） */
function reviewChip(raw, idx) {
  const r = parseReview(raw);
  if (!r) return `<span class="chip">—</span>`;
  const open = r.issues.filter((it) => it.status !== "fixed");
  if (!open.length) {
    const fixed = r.issues.length;
    return `<span class="chip chip-success" title="审校意见均已解决${fixed ? `（含已修复留档 ${fixed} 条）` : ""}">AI 已处理</span>`;
  }
  const mh = open.filter((it) => it.severity === "high" || it.severity === "medium").length;
  return `<span class="chip chip-warning chip-filter" data-review-open="${idx}" title="AI 审校仍有 ${open.length} 条意见未解决${mh ? `（其中高/中危 ${mh} 条）` : ""}：点击打开章节处理；也可用「AI 处理全部审校」批量重跑">余留 ${open.length} 条</span>`;
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
  const rd = hash.match(/^#\/read\/(.+)$/);
  if (rd) return { kind: "read", id: decodeURIComponent(rd[1]) };
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
  // 记住路由：Android 壳在 Activity 重建后会按此恢复页面（不至于跳回书架）
  try {
    if (window.AiNovelNative && typeof window.AiNovelNative.setRoute === "function") {
      window.AiNovelNative.setRoute(location.hash || "#/shelf");
    }
  } catch {
    /* 忽略：非安卓环境 */
  }

  const r = parseRoute();
  const activeNav = r.kind === "novel" || r.kind === "read" ? "#/shelf" : r.path;
  $$(".nav-item").forEach((a) => a.classList.toggle("active", a.dataset.route === activeNav));
  $$(".view").forEach((v) => (v.hidden = true));

  if (r.kind === "read") {
    $("#page-title").textContent = "阅读";
    $("#page-subtitle").textContent = "沉浸阅读（← 返回书架）";
    $("#view-read").hidden = false;
    try {
      await openRead(r.id);
    } catch (e) {
      toast("打开阅读失败：" + e.message, true);
      location.hash = "#/shelf";
    }
    return;
  }

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
    // 期间可能已切走：不遗留孤儿轮询（间隔内再自查一次，双保险）
    if (parseRoute().path !== "#/runs") return;
    state.pollTimer = setInterval(() => {
      if (parseRoute().path !== "#/runs") {
        clearInterval(state.pollTimer);
        state.pollTimer = null;
        return;
      }
      loadRuns().catch(() => {});
    }, 5000);
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
        <button class="btn btn-primary btn-sm" data-act="read"><svg class="ic"><use href="#i-book"/></svg>阅读</button>
        <button class="btn btn-outline btn-sm" data-act="open">进入工作台</button>
        <button class="btn btn-outline btn-sm" data-act="outline"><svg class="ic"><use href="#i-book"/></svg>总纲</button>
        <a class="btn btn-outline btn-sm" data-act="export" href="${esc(withToken(`/api/novels/${n.id}/export`))}">导出 TXT</a>
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
    if (!card) return;
    const id = card.dataset.id;
    const act = e.target.closest("[data-act]");
    if (!act) {
      // 点击卡片正文（非按钮区域）→ 直接进入阅读模式
      if (id && !card.classList.contains("empty-card")) location.hash = `#/read/${id}`;
      return;
    }
    const kind = act.dataset.act;
    if (kind === "read") {
      location.hash = `#/read/${id}`;
      return;
    }
    if (kind === "open") {
      location.hash = `#/novel/${id}`;
      return;
    }
    if (kind === "outline") {
      const item = state.shelfItems.find((x) => x.novel.id === id);
      if (item) openOutline(item);
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

/* ---------------- 作品总纲（书架卡：查看世界观 / 全书走向 / 立项要点） ---------------- */

function openOutline(item) {
  const n = item.novel || {};
  const title = (n.title && n.title.trim()) || (n.idea || "").slice(0, 16) || "（未命名）";
  $("#outline-title").textContent = `${title} · 总纲`;
  const chips = [];
  if (n.genre) chips.push(`<span class="chip">${esc(n.genre)}</span>`);
  if (n.style_preset) {
    const s = styleById(n.style_preset);
    chips.push(`<span class="chip">${esc(s ? s.name : n.style_preset)}</span>`);
  }
  if (n.mode === "staged") chips.push(`<span class="chip">按阶段确认</span>`);
  chips.push(statusChip(n.status));
  $("#outline-chips").innerHTML = chips.join("");
  const cell = (v) => esc(String(v)).replace(/\n/g, "<br>");
  // 立项澄清（作者已确认的设定）
  let clarify = [];
  try {
    clarify = JSON.parse(n.clarify_json || "[]");
  } catch {
    clarify = [];
  }
  const clarifyText = (Array.isArray(clarify) ? clarify : [])
    .filter((x) => x && String(x.answer || "").trim())
    .map((x) => `${x.question}：${x.answer}`)
    .join("\n");
  const rows = [
    ["世界观", n.worldview],
    ["全书走向", n.book_outline],
    ["一句话卖点", n.logline],
    ["主角", n.protagonist],
    ["写法基调", n.style],
    ["立项澄清", clarifyText],
    ["原始想法", n.idea],
  ].filter(([, v]) => v && String(v).trim());
  $("#outline-body").innerHTML = rows.length
    ? rows
        .map(([k, v]) => `<div class="outline-row"><div class="outline-k">${k}</div><div class="outline-v">${cell(v)}</div></div>`)
        .join("")
    : `<p class="muted" style="margin:0;">立项方案还没生成：点「进入工作台」开始创作后，AI 会补全世界观与全书走向。</p>`;
  $("#outline-progress").textContent =
    `已完成 ${item.chapters_done || 0} 章 · 共 ${item.words || 0} 字` +
    (item.chapters_target ? ` · 目标 ${item.chapters_target} 章` : "") +
    (item.run_status ? ` · ${RUN_STATUS[item.run_status] || item.run_status}` : "");
  $("#outline-open").onclick = () => {
    $("#outline-backdrop").hidden = true;
    location.hash = `#/novel/${n.id}`;
  };
  $("#outline-backdrop").hidden = false;
}

function bindOutline() {
  $("#outline-close").onclick = () => { $("#outline-backdrop").hidden = true; };
  $("#outline-backdrop").addEventListener("click", (e) => {
    if (e.target === $("#outline-backdrop")) $("#outline-backdrop").hidden = true;
  });
}

/* ---------------- 阅读模式（沉浸阅读：目录 / 上下章 / 字号 / 记忆进度） ---------------- */

const READ_POS_KEY = "ainovel.reading."; // + novelId → 章节号
const READ_FONT_KEY = "ainovel.reading.font";

function readFontSize() {
  let v = 18;
  try {
    v = parseInt(localStorage.getItem(READ_FONT_KEY) || "18", 10) || 18;
  } catch {
    v = 18;
  }
  return Math.min(26, Math.max(14, v));
}

function applyReadFont() {
  const el = $("#read-content");
  if (el) el.style.fontSize = readFontSize() + "px";
}

async function openRead(id) {
  state.readNovelId = id;
  const data = await api(`/api/novels/${id}`);
  state.readNovel = data.novel || {};
  state.readChapters = data.chapters || [];
  const title =
    (state.readNovel.title && state.readNovel.title.trim()) ||
    (state.readNovel.idea || "").slice(0, 16) ||
    "（未命名）";
  $("#read-title").textContent = title;
  $("#read-toc-list").hidden = true;
  applyReadFont();
  const readable = state.readChapters.filter((c) => (c.words || 0) > 0);
  if (!readable.length) {
    $("#read-content").innerHTML = `<h2>${esc(title)}</h2><p class="read-empty">这本书还没有可阅读的章节：先到工作台写一章吧。</p>`;
    $("#read-sub").textContent = "";
    $("#read-hint").textContent = "";
    $("#read-prev").disabled = true;
    $("#read-next").disabled = true;
    $("#read-toc-btn").disabled = true;
    return;
  }
  $("#read-toc-btn").disabled = false;
  let saved = 0;
  try {
    saved = parseInt(localStorage.getItem(READ_POS_KEY + id) || "0", 10) || 0;
  } catch {
    saved = 0;
  }
  // 续读上次位置；否则从第一章有正文的开始
  const target = readable.some((c) => c.idx === saved) ? saved : readable[0].idx;
  await readChapter(target);
}

async function readChapter(idx) {
  const id = state.readNovelId;
  if (!id) return;
  const readable = (state.readChapters || []).filter((c) => (c.words || 0) > 0);
  if (!readable.some((c) => c.idx === idx)) {
    toast(`第 ${idx} 章还没有正文`, true);
    return;
  }
  const c = await api(`/api/novels/${id}/chapters/${idx}`);
  state.readIdx = idx;
  try {
    localStorage.setItem(READ_POS_KEY + id, String(idx));
  } catch {
    /* 忽略 */
  }
  // 正文首行若是章节标题（生成契约），渲染为标题行；否则补一行
  const lines = String(c.content || "").split("\n");
  const first = (lines[0] || "").trim();
  const hasHead = first.length > 0 && first.length <= 30 && /^第\s*\S{0,10}章/.test(first);
  const body = hasHead ? lines.slice(1).join("\n") : String(c.content || "");
  const head = hasHead ? first : `第${idx}章 ${(c.title || "").trim()}`;
  $("#read-content").innerHTML = `<h2>${esc(head)}</h2>` + esc(body.trim()).replace(/\n/g, "<br>");
  const pos = readable.findIndex((x) => x.idx === idx);
  const totalChapters = (state.readChapters || []).length || 1;
  const pct = Math.min(100, Math.round(((pos + 1) / readable.length) * 100));
  $("#read-sub").textContent = `第 ${pos + 1} / ${readable.length} 章（全书 ${totalChapters} 章） · 本章 ${c.words || 0} 字 · 进度 ${pct}%`;
  $("#read-hint").textContent = `第 ${pos + 1} / ${readable.length} 章`;
  $("#read-prev").disabled = pos <= 0;
  $("#read-next").disabled = pos >= readable.length - 1;
  renderReadToc();
  const scroller = document.querySelector(".content");
  if (scroller) scroller.scrollTop = 0;
}

function renderReadToc() {
  const list = $("#read-toc-list");
  if (!list || list.hidden) return;
  list.innerHTML = (state.readChapters || [])
    .map((c) => {
      const readable = (c.words || 0) > 0;
      return `<div class="read-toc-item ${readable ? "" : "disabled"} ${c.idx === state.readIdx ? "active" : ""}" data-toc="${c.idx}">
        <span>第${c.idx}章 ${esc(c.title || "")}</span>
        <span class="muted">${readable ? `${c.words} 字` : "未写"}</span>
      </div>`;
    })
    .join("");
}

function readStep(delta) {
  const readable = (state.readChapters || []).filter((c) => (c.words || 0) > 0);
  const pos = readable.findIndex((x) => x.idx === state.readIdx);
  const next = pos + delta;
  if (pos < 0 || next < 0 || next >= readable.length) return;
  readChapter(readable[next].idx).catch((e) => toast(e.message, true));
}

function bindRead() {
  $("#read-back").onclick = () => { location.hash = "#/shelf"; };
  $("#read-prev").onclick = () => readStep(-1);
  $("#read-next").onclick = () => readStep(1);
  $("#read-toc-btn").onclick = () => {
    const list = $("#read-toc-list");
    list.hidden = !list.hidden;
    if (!list.hidden) renderReadToc();
  };
  $("#read-toc-list").addEventListener("click", (e) => {
    const item = e.target.closest("[data-toc]");
    if (!item || item.classList.contains("disabled")) return;
    $("#read-toc-list").hidden = true;
    readChapter(parseInt(item.dataset.toc, 10)).catch((err) => toast(err.message, true));
  });
  $("#read-font-dec").onclick = () => {
    try {
      localStorage.setItem(READ_FONT_KEY, String(Math.max(14, readFontSize() - 1)));
    } catch { /* 忽略 */ }
    applyReadFont();
  };
  $("#read-font-inc").onclick = () => {
    try {
      localStorage.setItem(READ_FONT_KEY, String(Math.min(26, readFontSize() + 1)));
    } catch { /* 忽略 */ }
    applyReadFont();
  };
  // 键盘翻页（桌面 / 横屏带键盘时）
  document.addEventListener("keydown", (e) => {
    if (parseRoute().kind !== "read") return;
    if (e.target && /^(INPUT|TEXTAREA|SELECT)$/.test(e.target.tagName)) return;
    if (e.key === "ArrowLeft") { e.preventDefault(); readStep(-1); }
    if (e.key === "ArrowRight") { e.preventDefault(); readStep(1); }
    if (e.key === "Escape") location.hash = "#/shelf";
  });
}

/* ---------------- 新建作品 ---------------- */

/** 立项澄清：AI 提问 → 作者回答（答案随建书提交，作为「已确认的设定」约束立项） */
async function runClarify() {
  const idea = $("#create-idea").value.trim();
  if (!idea) {
    toast("请先写一句灵感，再让 AI 提问", true);
    return;
  }
  const btn = $("#create-clarify");
  btn.disabled = true;
  $("#create-clarify-status").textContent = "AI 正在分析灵感、提炼关键问题…";
  try {
    const r = await apiSend("/api/clarify", "POST", { idea, style_preset: getStylePick() });
    renderClarifyQuestions(r.questions || []);
    $("#create-clarify-clear").hidden = false;
  } catch (err) {
    toast(err.message, true);
    $("#create-clarify-status").textContent = "提问失败：" + err.message;
  } finally {
    btn.disabled = false;
  }
}

function renderClarifyQuestions(questions) {
  const box = $("#create-clarify-list");
  if (!questions.length) {
    box.innerHTML = "";
    $("#create-clarify-status").textContent = "没有生成问题，可直接开始";
    return;
  }
  $("#create-clarify-status").textContent = `共 ${questions.length} 个问题：点建议选项可直接填入，也可以自由回答；不回答的会被忽略`;
  box.innerHTML = questions
    .map(
      (q, i) => `
      <div class="panel" style="padding:10px 12px;margin:0 0 8px;">
        <div style="font-size:13px;font-weight:600;">${i + 1}. ${esc(q.question || "")}</div>
        ${q.hint ? `<div class="muted" style="font-size:12px;margin-top:2px;">${esc(q.hint)}</div>` : ""}
        ${(q.suggestions || []).length
          ? `<div class="chips" style="margin-top:6px;">${q.suggestions
              .map((s) => `<span class="chip chip-filter" data-clarify-suggest="${i}" data-value="${esc(s)}">${esc(s)}</span>`)
              .join("")}</div>`
          : ""}
        <input class="input-sm" style="margin-top:8px;width:100%;" data-clarify-answer="${i}" placeholder="你的回答…" />
      </div>`,
    )
    .join("");
}

/** 从表单读取已作答的澄清问答（未作答的忽略） */
function collectClarify() {
  const out = [];
  const items = $$("#create-clarify-list [data-clarify-answer]");
  items.forEach((el) => {
    const i = parseInt(el.dataset.clarifyAnswer, 10);
    const answer = (el.value || "").trim();
    if (!answer) return;
    const card = el.closest(".panel");
    const question = card ? card.querySelector("div")?.textContent?.replace(/^\d+\.\s*/, "") || "" : "";
    out.push({ question, answer });
  });
  return out;
}

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
  $("#create-clarify").onclick = () => runClarify();
  $("#create-clarify-clear").onclick = () => {
    $("#create-clarify-list").innerHTML = "";
    $("#create-clarify-clear").hidden = true;
    $("#create-clarify-status").textContent = "动笔前先把核心设定问清楚（可选；也可以直接开始）";
  };
  // 建议选项：点一下填入回答框
  $("#create-clarify-list").addEventListener("click", (e) => {
    const chip = e.target.closest("[data-clarify-suggest]");
    if (!chip) return;
    const input = $(`#create-clarify-list [data-clarify-answer="${chip.dataset.clarifySuggest}"]`);
    if (input) {
      input.value = chip.dataset.value || "";
      input.focus();
    }
  });
  $("#create-form").addEventListener("submit", async (e) => {
    e.preventDefault();
    const idea = $("#create-idea").value.trim();
    if (!idea) { toast("请先写一句灵感", true); return; }
    const chapters = Math.min(999, Math.max(1, parseInt($("#create-chapters").value || "3", 10) || 3));
    const mode = $("#create-mode").value;
    const style_preset = getStylePick();
    const clarify = collectClarify();
    const btn = $("#create-submit");
    btn.disabled = true;
    btn.textContent = "正在创建…";
    try {
      const r = await apiSend("/api/novels", "POST", { idea, chapters, mode, style_preset, clarify });
      const extra = clarify.length ? `（已带入 ${clarify.length} 条确认设定）` : "";
      const first = r.first_batch && r.first_batch < chapters ? `首批先写 ${r.first_batch} 章（目标 ${chapters} 章）` : "";
      toast(
        (mode === "staged"
          ? "已开始：规划完成后会暂停等你确认"
          : `已开始创作${first ? "，" + first : ""}，进入工作台查看进度`) + extra,
      );
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
  // 实时阶段 → 步骤高亮（比落库的 run.stage 更细：review/autofix/extract 都归到第 4 步）
  const STAGE_TO_STEP = { brief: 0, outline: 1, draft: 2, review: 3, autofix: 3, extract: 3 };
  let activeIdx = -1;
  if (running || paused) {
    const live = state.liveStage && STAGE_TO_STEP[state.liveStage];
    activeIdx = live !== undefined && live !== null ? live : steps.findIndex((s) => !s.done);
    // 已有进度的后续阶段不应倒退：若实时步骤已完成，取第一个未完成
    if (activeIdx >= 0 && steps[activeIdx].done) {
      const first = steps.findIndex((s) => !s.done);
      activeIdx = first === -1 ? activeIdx : first;
    }
  }
  return steps.map((s, i) => ({ ...s, state: s.done ? "done" : i === activeIdx ? "active" : "pending" }));
}

function renderNovel() {
  const data = state.novel;
  if (!data) return;
  const n = data.novel;
  const chs = data.chapters || [];
  const run = data.run;

  // 切换作品时清掉上一本书的检索词与结果（避免显示错书的命中）
  if (state.renderedNovelId !== n.id) {
    state.renderedNovelId = n.id;
    const searchInput = $("#chapter-search");
    const searchBox = $("#chapter-search-results");
    if (searchInput) searchInput.value = "";
    if (searchBox) {
      searchBox.hidden = true;
      searchBox.innerHTML = "";
    }
  }

  $("#novel-title").textContent = (n.title && n.title.trim()) || "（未命名）";
  $("#novel-chips").innerHTML = [
    n.genre ? `<span class="chip">${esc(n.genre)}</span>` : "",
    statusChip(n.status),
    run ? `<span class="chip ${run.status === "failed" ? "chip-danger" : run.status === "paused" ? "chip-warning" : run.status === "done" ? "chip-success" : "chip-info"}">${esc(RUN_STATUS[run.status] || run.status)}</span>` : "",
    `<span class="chip chip-filter ${n.style_preset ? "" : "muted-chip"}" id="novel-style-chip" title="点击更换文风预设">${ic("i-pen")}文风：${esc(styleName(n.style_preset))}</span>`,
  ].join("");
  $("#novel-meta").textContent = [n.logline, n.protagonist && `主角：${n.protagonist}`, n.style].filter(Boolean).join(" · ") || n.idea || "";

  const target = n.target_chapters > 0 ? n.target_chapters : (run && run.chapters_target) || Math.max(chs.length, 3);
  if (!$("#novel-target").dataset.touched) {
    $("#novel-target").value = target;
  }
  $("#novel-export").href = withToken(`/api/novels/${n.id}/export`);

  const running = !!(run && run.status === "running");
  const paused = !!(run && run.status === "paused");
  const stopped = paused && run.stage === "stopped";
  const awaiting = paused && !stopped;
  const runBtn = $("#novel-run");
  runBtn.disabled = running;
  runBtn.textContent = running ? "创作中…" : awaiting ? "确认并继续" : "继续创作";
  runBtn.classList.toggle("attention", awaiting);

  // 停止按钮：仅创作运行时可用
  const stopBtn = $("#novel-stop");
  if (stopBtn) stopBtn.disabled = !running;

  // 自动连载按钮：连载中显示「停止连载」
  const autoBtn = $("#novel-auto");
  const autoOn = !!n.auto_continue;
  autoBtn.innerHTML = autoOn
    ? `${ic("i-x")}停止连载`
    : `${ic("i-refresh")}自动连载`;
  autoBtn.classList.toggle("btn-danger", autoOn);
  autoBtn.title = autoOn
    ? "停止自动连载：当前批次写完后不再自动续写"
    : `自动连载：每批最多 10 章，写完自动续写直到目标 ${target} 章（遇失败/预算上限自动暂停）`;

  // 推进方式切换
  $("#mode-toggle").innerHTML = `
    <span class="chip chip-filter ${n.mode === "auto" ? "chip-dark" : ""}" data-mode="auto" title="全自动推进">全自动</span>
    <span class="chip chip-filter ${n.mode === "staged" ? "chip-dark" : ""}" data-mode="staged" title="规划后暂停确认">阶段确认</span>`;

  // 进度
  const done = chs.filter((c) => c.status === "done").length;
  const pct = target > 0 ? Math.min(100, Math.round((done / target) * 100)) : 0;
  $("#run-chips").innerHTML = [
    run ? `<span class="chip ${run.status === "failed" ? "chip-danger" : paused ? "chip-warning" : run.status === "done" ? "chip-success" : "chip-info"}">${esc(stopped ? "已停止" : RUN_STATUS[run.status] || run.status)}</span>` : `<span class="chip">尚未运行</span>`,
    `<span class="chip">进度 ${done}/${target} 章</span>`,
    run ? `<span class="chip">当前阶段：${esc(STAGE_LABEL[run.stage] || run.stage)}</span>` : "",
  ].join("");
  $("#run-progress").style.width = pct + "%";
  $("#run-progress").classList.toggle("running", running);
  $("#run-note").textContent = run && run.error
    ? "已中断：" + run.error
    : awaiting
      ? "已暂停：章节规划已生成。到下方「章节」里检查/修改章纲，然后点「确认并继续」开始写正文。"
      : stopped
        ? "已停止：上次创作被手动停止，已写章节全部保留。点「继续创作」可随时断点续写。"
        : running
          ? "AI 正在按「立项 → 规划 → 正文 → 审校」推进，可离开本页，任务在后台继续；点「停止」可中断（已写章节保留）。"
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

/** 头部概览：未回收伏笔数（提示用）+ AI 审校处理量（只报"已处理多少"，不报待办） */
async function refreshInsights() {
  if (!state.novelId) return;
  const wantId = state.novelId;
  let openF = 0;
  try {
    const f = await api(`/api/novels/${wantId}/foreshadowings`);
    if (state.novelId !== wantId) return;
    state.foreshadowings = f.items || [];
    openF = state.foreshadowings.filter((x) => x.status === "open").length;
  } catch { /* 忽略 */ }
  let fixCalls = 0;
  try {
    const u = await api(`/api/novels/${wantId}/usage`);
    fixCalls = (u.by_task || [])
      .filter((g) => g.key === "auto_fix")
      .reduce((sum, g) => sum + (g.calls || 0), 0);
  } catch { /* 忽略 */ }
  if (parseRoute().kind !== "novel" || state.novelId !== wantId) return;
  const reviewed = (state.novel?.chapters || []).filter((c) => c.review).length;
  // 与章节表同口径：统计所有未解决意见（fixed 视为已解决）
  let openTotal = 0;
  for (const c of state.novel?.chapters || []) {
    const r = parseReview(c.review);
    if (r) openTotal += r.issues.filter((it) => it.status !== "fixed").length;
  }
  const el = $("#novel-insights");
  if (!el) return;
  el.innerHTML =
    `<span class="chip chip-filter ${openF ? "chip-warning" : ""}" data-insight="foreshadowings" title="点击查看伏笔台账">${ic("i-flag")}未回收伏笔 ${openF}</span>` +
    `<span class="chip chip-filter ${openTotal ? "chip-warning" : ""}" data-insight="chapters" title="${openTotal ? "还有未解决的审校意见：点章节表里的「余留 N 条」逐章处理，或点「AI 处理全部审校」批量重跑" : "所有审校意见均已解决"}">${ic("i-check")}AI 审校已覆盖 ${reviewed} 章${openTotal ? ` · 余留 ${openTotal} 条` : " · 全部解决"}${fixCalls ? ` · 修复 ${fixCalls} 次` : ""}</span>`;
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
  loadHealthExtras().catch(() => {});
}

/** 书籍体检扩展块：卷末体检报告 + 文体统计（本地零 token；失败不影响主体检） */
async function loadHealthExtras() {
  const id = state.novelId;
  $("#health-volume-reports").textContent = "加载中…";
  $("#health-style-stats").textContent = "加载中…";
  // 文体统计
  try {
    const s = await api(`/api/novels/${id}/style-stats`);
    let html = `<div class="chips"><span class="chip">篇章 ${s.chapters}</span><span class="chip">字数中位数 ${s.median_words}</span>`;
    for (const o of (s.word_outliers || []).slice(0, 6)) {
      html += `<span class="chip chip-warning">第${o.idx}章 ${esc(o.kind)}（${o.words}字）</span>`;
    }
    html += `</div>`;
    const reps = s.repeated_sentences || [];
    if (reps.length) {
      html += `<p class="muted" style="margin:8px 0 2px;">跨章重复句（Top ${Math.min(8, reps.length)}；连续的重复句式是 AI 味头号特征）：</p>`;
      html += reps
        .slice(0, 8)
        .map(
          (r) =>
            `<p class="health-line">${ic("i-flag")}<span>「${esc(String(r.sentence).slice(0, 42))}」× ${r.count} 章（第 ${(r.chapters || []).join("、")} 章）</span></p>`,
        )
        .join("");
    } else if (s.chapters > 0) {
      html += `<p class="muted" style="margin:6px 0 0;">未发现跨章重复句。</p>`;
    }
    if (s.same_ending_ratio >= 0.7 && s.chapters >= 4) {
      html += `<p class="health-line">${ic("i-flag")}<span>章末类型偏单一（最高占比 ${Math.round(s.same_ending_ratio * 100)}%）：建议变换断章方式（动作截断/疑问/信息投放交替）</span></p>`;
    }
    if (state.novelId !== id) return;
    $("#health-style-stats").innerHTML = html;
  } catch (e) {
    $("#health-style-stats").textContent = "加载失败：" + e.message;
  }
  // 卷末体检报告
  try {
    const r = await api(`/api/novels/${id}/volume-reports`);
    const items = r.items || [];
    if (state.novelId !== id) return;
    if (!items.length) {
      $("#health-volume-reports").innerHTML = `<p class="muted" style="margin:0;">写满 20 章后自动生成第一卷体检；缺失的报告会在下次创作/巡检时自动补跑。</p>`;
    } else {
      $("#health-volume-reports").innerHTML = items
        .map((v) => {
          const findings = (v.report && v.report.findings) || [];
          const head = `<div class="chips"><span class="chip chip-info">第 ${v.volume_idx} 卷（至第 ${v.upto_chapter} 章）</span>${
            findings.length
              ? `<span class="chip chip-warning">${findings.length} 条建议</span>`
              : `<span class="chip chip-success">无明显问题</span>`
          }</div>`;
          const lines = findings
            .map((f) => {
              const chs = (f.chapters || []).length ? `（第 ${(f.chapters || []).join("、")} 章）` : "";
              const sug = f.suggestion ? ` → ${esc(f.suggestion)}` : "";
              return `<p class="health-line">${ic("i-flag")}<span>[${esc(f.severity || "")}] ${esc(f.note || "")}${chs}${sug}</span></p>`;
            })
            .join("");
          return `<div style="margin:8px 0;">${head}${lines}</div>`;
        })
        .join("");
    }
  } catch (e) {
    $("#health-volume-reports").textContent = "加载失败：" + e.message;
  }
}

/** 读者视角评审：按区间调用（黄金三章 / 前 10 / 最近 10） */
async function runReaderReview() {
  const pick = $("#readerv-range").value;
  let from = 1;
  let to = 3;
  if (pick === "first10") {
    to = 10;
  } else if (pick === "last10") {
    const done = (state.novel?.chapters || []).filter((c) => c.status === "done" && (c.words || 0) > 0);
    if (!done.length) {
      toast("还没有已写正文的章节", true);
      return;
    }
    to = done[done.length - 1].idx;
    from = Math.max(1, to - 9);
  }
  const btn = $("#readerv-run");
  btn.disabled = true;
  $("#readerv-status").textContent = "AI 正在以读者视角评审…";
  try {
    const r = await apiSend(`/api/novels/${state.novelId}/reader-review`, "POST", { from, to });
    const res = r.result || {};
    const chapters = res.chapters || [];
    $("#readerv-result").innerHTML =
      `<div class="chips"><span class="chip chip-info">总评 ${esc(String(res.overall_score ?? "—"))}/10</span>${r.golden ? `<span class="chip chip-warning">黄金三章专项</span>` : ""}</div>` +
      `<p class="muted" style="margin:6px 0;">${esc(res.overall_comment || "")}</p>` +
      chapters
        .map(
          (c) =>
            `<p class="health-line">${ic("i-flag")}<span>第 ${c.idx} 章 ${esc(String(c.score ?? "—"))}/10：${esc(c.verdict || "")}${
              (c.issues || []).length ? `（${(c.issues || []).map(esc).join("；")}）` : ""
            }</span></p>`,
        )
        .join("");
    $("#readerv-status").textContent = `已完成（第 ${r.from}~${r.to} 章，覆盖 ${r.included} 章）`;
  } catch (err) {
    $("#readerv-status").textContent = "评审失败：" + err.message;
  } finally {
    btn.disabled = false;
  }
}

async function fillMissing() {
  const btn = $("#health-fill");
  btn.disabled = true;
  $("#health-status").textContent = "已开始开工巡检（状态修正 / 补摘要 / 漏审修复）…";
  try {
    await apiSend(`/api/novels/${state.novelId}/maintenance`, "POST");
    toast("开工巡检已在后台开始，进度与结果见「跟随生成」日志");
    $("#health-status").textContent = "巡检进行中：完成后日志会给出汇总";
  } catch (err) {
    toast(err.message, true);
    $("#health-status").textContent = "巡检失败：" + err.message;
  } finally {
    btn.disabled = false;
  }
}

/* ---------------- AI 用量 ---------------- */

const USAGE_TASK_LABEL = {
  clarify: "立项澄清",
  brief: "立项方案",
  outline: "章节规划",
  chapter: "正文生成",
  chapter_retry: "文风重写",
  volume_review: "卷末体检",
  reader_review: "读者视角评审",
  auto_fix: "自动修复",
  review: "章节审校",
  polish: "选段修文",
  continue: "光标续写",
  rewrite: "重写本章",
  extract: "资料整理",
  foreshadow_audit: "伏笔审核",
  volume_summary: "卷级摘要",
  global_review: "全局审校",
  global_fix: "全局修复",
  test: "连通测试",
};

function fmtTokens(v) {
  if (v === null || v === undefined) return "—";
  return String(v);
}

async function openUsage() {
  const u = await api(`/api/novels/${state.novelId}/usage`);
  let month = null;
  try {
    month = await api("/api/usage");
  } catch {
    /* 忽略：月度护栏数据获取失败不影响明细 */
  }
  const t = u.totals || {};
  const hitRate =
    t.input && t.cache_hit !== null && t.cache_hit !== undefined
      ? `（${Math.round((t.cache_hit / t.input) * 100)}%）`
      : "";
  $("#usage-stats").innerHTML = [
    month
      ? `<span class="chip ${month.over_budget ? "chip-danger" : "chip-info"}">本月累计 ${fmtTokens(month.used)}${
          month.budget > 0 ? ` / 预算 ${fmtTokens(month.budget)}` : "（预算不限）"
        }</span>`
      : "",
    `<span class="chip">本书调用 ${t.calls || 0} 次</span>`,
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
        <td>${esc(c.title || `第${c.idx}章`)}${c.hook_type || c.intensity > 0 ? `<span class="chip chip-info" style="margin-left:6px;font-size:11px;" title="章末钩型与推进强度（AI 章纲规划；可在「改规划」里调整）">${esc(c.hook_type || "钩")}${c.intensity > 0 ? `·${c.intensity}` : ""}</span>` : ""}</td>
        <td>${c.status === "done" ? `<span class="chip chip-success">已完成</span>` : c.status === "drafted" ? `<span class="chip chip-info">待审校</span>` : `<span class="chip">已规划</span>`}</td>
        <td class="num">${c.words || 0}</td>
        <td>${reviewChip(c.review, c.idx)}</td>
        <td class="row-actions">
          ${c.status === "planned"
            ? `<button class="btn btn-outline btn-sm" data-plan="${c.idx}">改规划</button>`
            : `<button class="icon-btn" data-read="${c.idx}" title="阅读 / 编辑">${ic("i-eye")}</button>`}
          <button class="icon-btn" data-copy="${c.idx}" title="一键复制（标题+正文，粘贴到作家平台）">${ic("i-copy")}</button>
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
        <td><span class="chip ${f.status === "open" ? "chip-warning" : "chip-success"}">${f.status === "open" ? "未回收" : "已回收"}</span>${
          f.status !== "open" && /AI 自动审核/.test(f.notes || "")
            ? `<span class="chip chip-info" title="${esc(f.notes)}">AI 审核</span>`
            : ""
        }</td>
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
  // 全文检索（FTS5，服务端建索引）
  const searchInput = $("#chapter-search");
  const searchBox = $("#chapter-search-results");
  if (searchInput && searchBox) {
    let timer = null;
    let seq = 0;
    searchInput.addEventListener("input", () => {
      clearTimeout(timer);
      const q = searchInput.value.trim();
      if (!q) {
        searchBox.hidden = true;
        searchBox.innerHTML = "";
        return;
      }
      const wantNovel = state.novelId;
      const mySeq = ++seq;
      timer = setTimeout(async () => {
        try {
          const r = await api(`/api/novels/${wantNovel}/search?q=${encodeURIComponent(q)}`);
          // 只接受「最后一次输入」且仍在本作品页时的响应（慢请求不覆盖新结果）
          if (mySeq !== seq || state.novelId !== wantNovel) return;
          const items = r.items || [];
          searchBox.hidden = false;
          searchBox.innerHTML = items.length
            ? items
                .map(
                  (it) =>
                    `<div class="search-hit" data-idx="${it.idx}"><span class="chip chip-info">第${it.idx}章</span><span class="search-hit-title">${esc(it.title || "")}</span><span class="search-hit-excerpt muted">${esc(it.excerpt || "")}</span></div>`,
                )
                .join("")
            : `<p class="muted" style="margin:0;">没有找到「${esc(q)}」</p>`;
        } catch (err) {
          toast(err.message, true);
        }
      }, 220);
    });
    searchBox.addEventListener("click", (e) => {
      const hit = e.target.closest("[data-idx]");
      if (!hit) return;
      searchBox.hidden = true;
      openEditor(parseInt(hit.dataset.idx, 10)).catch((err) => toast(err.message, true));
    });
  }
  $("#chapter-rows").addEventListener("click", async (e) => {
    const read = e.target.closest("[data-read]");
    if (read) { openEditor(parseInt(read.dataset.read, 10)).catch((err) => toast(err.message, true)); return; }
    const rev = e.target.closest("[data-review-open]");
    if (rev) { openEditor(parseInt(rev.dataset.reviewOpen, 10)).catch((err) => toast(err.message, true)); return; }
    const copy = e.target.closest("[data-copy]");
    if (copy) { copyChapter(parseInt(copy.dataset.copy, 10)).catch((err) => toast(err.message, true)); return; }
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
  $("#novel-target").addEventListener("change", async (e) => {
    const v = Math.min(999, Math.max(1, parseInt(e.target.value || "1", 10) || 1));
    e.target.value = v;
    e.target.dataset.touched = "";
    try {
      await apiSend(`/api/novels/${state.novelId}`, "PATCH", { target_chapters: v });
      toast(`目标章节数已设为 ${v} 章`);
      await refreshNovel();
    } catch (err) {
      toast(err.message, true);
    }
  });
  $("#novel-auto").onclick = async () => {
    const on = !!state.novel?.novel?.auto_continue;
    try {
      if (on) {
        await api(`/api/novels/${state.novelId}/auto`, { method: "DELETE" });
        toast("已停止自动连载（当前批次写完后不再续写）");
      } else {
        const r = await api(`/api/novels/${state.novelId}/auto`, { method: "POST" });
        toast(`自动连载已开始：每批 ≤10 章，直到第 ${r.target} 章`);
      }
      await refreshNovel();
    } catch (err) {
      toast(err.message, true);
    }
  };
  $("#novel-review-all").onclick = async () => {
    try {
      const r = await api(`/api/novels/${state.novelId}/review-all`, { method: "POST" });
      toast(r.queued > 0 ? `已开始：AI 后台处理 ${r.queued} 章审校（进度见「跟随生成」）` : "章节审校均已处理，正在自动核对伏笔回收");
      await refreshNovel();
    } catch (err) {
      toast(err.message, true);
    }
  };
  $("#novel-run").onclick = async () => {
    try {
      const r = await apiSend(`/api/novels/${state.novelId}/run`, "POST", {});
      toast(`已开始：再写一批（至第 ${r.batch_until} 章；已完成章节自动跳过）`);
      $("#novel-run").disabled = true;
      $("#novel-run").textContent = "创作中…";
      $("#novel-run").classList.remove("attention");
      const stopBtn = $("#novel-stop");
      if (stopBtn) stopBtn.disabled = false;
    } catch (err) {
      toast(err.message, true);
      refreshNovel();
    }
  };
  $("#novel-stop").onclick = async () => {
    const btn = $("#novel-stop");
    btn.disabled = true;
    try {
      const r = await apiSend(`/api/novels/${state.novelId}/stop`, "POST");
      toast(
        r.stopping
          ? "已请求停止：生成随即将中断，已写章节全部保留"
          : "当前没有进行中的创作（已关闭自动连载）",
      );
      await refreshNovel();
    } catch (err) {
      toast(err.message, true);
      btn.disabled = false;
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
  $("#readerv-run").onclick = () => runReaderReview();
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
      {
        name: "hook_type",
        label: "章末钩型（节奏控制：每 2~3 章一个高能钩，别连着用同一型）",
        type: "select",
        value: c.hook_type || "",
        options: [
          { value: "", label: "（未标注）" },
          { value: "悬念", label: "悬念（疑问绑定 / 信息截断）" },
          { value: "危机", label: "危机（动作截断 / 命悬一线）" },
          { value: "反转", label: "反转（推翻本章结论）" },
          { value: "升级", label: "升级（规则 / 能力变化）" },
          { value: "信息", label: "信息（关键信息投放）" },
        ],
      },
      {
        name: "intensity",
        label: "推进强度（节奏曲线：连续低强度不超过 2 章）",
        type: "select",
        value: String(c.intensity || 0),
        options: [
          { value: "0", label: "未标注" },
          { value: "1", label: "1 过渡" },
          { value: "2", label: "2 平缓推进" },
          { value: "3", label: "3 小高潮" },
          { value: "4", label: "4 强推进" },
          { value: "5", label: "5 卷级爆点" },
        ],
      },
    ],
    onSubmit: async (v) => {
      if (!v.title.trim()) throw new Error("标题不能为空");
      const beats = v.beats.split("\n").map((s) => s.trim()).filter(Boolean);
      await apiSend(`/api/novels/${state.novelId}/chapters/${idx}/plan`, "PUT", {
        title: v.title,
        goal: v.goal,
        beats,
        hook_type: v.hook_type || "",
        intensity: parseInt(v.intensity || "0", 10) || 0,
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

/* ---------------- 一键复制（粘贴到作家平台） ---------------- */

/** 复制到剪贴板：优先 Clipboard API，回退 execCommand（WebView 两种都可用） */
async function copyText(text) {
  try {
    if (navigator.clipboard && navigator.clipboard.writeText) {
      await navigator.clipboard.writeText(text);
      return true;
    }
  } catch {
    /* 回退 */
  }
  try {
    const ta = document.createElement("textarea");
    ta.value = text;
    ta.setAttribute("readonly", "");
    ta.style.position = "fixed";
    ta.style.opacity = "0";
    document.body.appendChild(ta);
    ta.select();
    const ok = document.execCommand("copy");
    ta.remove();
    return ok;
  } catch {
    return false;
  }
}

/** 章节导出文本：第一行「第N章 标题」，空行后正文 —— 直接粘贴即可 */
function chapterExportText(idx, title, content) {
  const name = (title || "").trim();
  const head = name
    ? (/^第\s*\S{0,10}章/.test(name) ? name : `第${idx}章 ${name}`)
    : `第${idx}章`;
  return `${head}\n\n${(content || "").trim()}\n`;
}

/** 列表里一键复制（取已保存正文） */
async function copyChapter(idx) {
  const c = await api(`/api/novels/${state.novelId}/chapters/${idx}`);
  if (!c.content || !c.content.trim()) throw new Error(`第${idx}章还没有正文`);
  const ok = await copyText(chapterExportText(idx, c.title, c.content));
  if (!ok) throw new Error("复制失败（剪贴板不可用）");
  toast(`已复制第${idx}章（标题+正文），可直接粘贴到作家平台`);
}

/* ---------------- 批量操作（删除范围 / 从某章之后重写） ---------------- */

function batchLastIdx() {
  const chs = state.novel?.chapters || [];
  return chs.length ? chs[chs.length - 1].idx : 0;
}

function updateBatchHint() {
  const chs = state.novel?.chapters || [];
  const last = batchLastIdx();
  const n = parseInt($("#batch-rewrite-n").value || "0", 10) || 0;
  const replan = $("#batch-replan").checked;
  const count = chs.filter((c) => c.idx > n).length;
  $("#batch-rewrite-hint").textContent = count
    ? `将重写第 ${n + 1} ~ ${last} 章，共 ${count} 章；旧稿自动留档可回滚${replan ? "；章纲会重新规划（标题/目标/节拍重生成）" : "，章纲保留"}`
    : "该章之后没有可重写的章节";
}

function openBatch() {
  const last = batchLastIdx();
  if (!last) throw new Error("还没有章节");
  $("#batch-rewrite-n").value = last > 1 ? last - 1 : 0;
  $("#batch-replan").checked = false;
  $("#batch-del-from").value = 1;
  $("#batch-del-to").value = last;
  $("#batch-del-status").textContent = "";
  updateBatchHint();
  $("#batch-backdrop").hidden = false;
}

function bindBatch() {
  $("#novel-batch").onclick = () => openBatch();
  $("#batch-close").onclick = () => { $("#batch-backdrop").hidden = true; };
  $("#batch-backdrop").addEventListener("click", (e) => {
    if (e.target === $("#batch-backdrop")) $("#batch-backdrop").hidden = true;
  });
  $("#batch-rewrite-n").addEventListener("input", updateBatchHint);
  $("#batch-replan").addEventListener("change", updateBatchHint);
  $("#batch-rewrite-go").onclick = async () => {
    const btn = $("#batch-rewrite-go");
    const n = Math.max(0, parseInt($("#batch-rewrite-n").value || "0", 10) || 0);
    const replan = $("#batch-replan").checked;
    const last = batchLastIdx();
    const count = (state.novel?.chapters || []).filter((c) => c.idx > n).length;
    if (!count) {
      toast("该章之后没有可重写的章节", true);
      return;
    }
    const extra = replan ? "并重新规划章纲（原标题/目标/节拍会被替换）" : "（章纲保留）";
    if (!confirm(`确认从第 ${n + 1} 章起重写 ${count} 章${extra}？旧稿会自动留档，之后可回滚。`)) return;
    btn.disabled = true;
    try {
      const r = await apiSend(`/api/novels/${state.novelId}/chapters/rewrite-after`, "POST", {
        after_idx: n,
        replan,
      });
      toast(`已开始重写第 ${r.from} ~ ${r.to} 章（共 ${r.count} 章），进度见「跟随生成」`);
      $("#batch-backdrop").hidden = true;
      await refreshNovel();
    } catch (err) {
      toast(err.message, true);
    } finally {
      btn.disabled = false;
    }
  };
  $("#batch-del-go").onclick = async () => {
    const btn = $("#batch-del-go");
    const last = batchLastIdx();
    const from = Math.max(1, parseInt($("#batch-del-from").value || "1", 10) || 1);
    const to = Math.min(last, parseInt($("#batch-del-to").value || String(last), 10) || last);
    if (to < from) {
      toast("范围不合法：起始章号不能大于结束章号", true);
      return;
    }
    const count = to - from + 1;
    if (!confirm(`确认删除第 ${from} ~ ${to} 章（共 ${count} 章）？该操作不可恢复，后续章节会自动重排。`)) return;
    btn.disabled = true;
    try {
      const r = await apiSend(`/api/novels/${state.novelId}/chapters/batch-delete`, "POST", { from, to });
      toast(`已删除 ${r.deleted} 章，后续章节已重排（台账 / 全局审校引用已同步）`);
      $("#batch-del-status").textContent = `已删除第 ${from} ~ ${to} 章`;
      await refreshNovel();
      openBatch();
    } catch (err) {
      toast(err.message, true);
    } finally {
      btn.disabled = false;
    }
  };
}

/* ---------------- 章节历史（版本回滚 + AI 复原） ---------------- */

let historyIdx = 0;

/** 快照来源标签（pre-X = 在 X 操作之前的版本） */
const VERSION_SOURCE = {
  "pre-draft": "生成前",
  "pre-autofix": "审校修复前",
  "pre-retry": "文风重写前",
  "pre-rewrite": "重写前",
  "pre-manual": "手动编辑前",
  "pre-global-fix": "全局修复前",
  "pre-review-all": "审校处理前",
  "pre-restore": "复原操作前",
  "pre-ai-restore": "AI 复原前",
  "pre-auto": "修改前",
};

async function openHistory() {
  const ta = $("#reader-content");
  const idx = parseInt(ta.dataset.idx || "0", 10);
  if (!idx) return;
  historyIdx = idx;
  $("#history-backdrop").hidden = false;
  $("#history-preview").hidden = true;
  $("#history-preview").textContent = "";
  $("#history-status").textContent = "";
  await refreshHistory();
}

async function refreshHistory() {
  const r = await api(`/api/novels/${state.novelId}/chapters/${historyIdx}/versions`);
  const cur = r.current || {};
  $("#history-current").innerHTML =
    `<span class="chip chip-dark">当前稿 · ${cur.words || 0} 字 · 综合分 ${cur.score ?? "—"}</span>` +
    (cur.open_issues
      ? `<span class="chip chip-warning">仍存在 ${cur.open_issues} 条高/中危意见</span>`
      : `<span class="chip chip-success">无高/中危意见</span>`);
  const items = r.items || [];
  $("#history-rows").innerHTML = items.length
    ? items
        .map((v) => {
          const label = VERSION_SOURCE[v.source] || v.source || "历史版本";
          const better = (v.score ?? -999) > (cur.score ?? -999);
          return `<tr>
            <td class="num">${fmtTime(v.created_at)}</td>
            <td>${esc(label)}${
              v.open_issues
                ? `<span class="chip chip-warning" style="margin-left:6px;">问题 ${v.open_issues}</span>`
                : `<span class="chip chip-success" style="margin-left:6px;">0 问题</span>`
            }</td>
            <td class="num">${v.words}</td>
            <td class="num">${v.score}${better ? `<span class="chip chip-info" style="margin-left:6px;">更优</span>` : ""}</td>
            <td>
              <button class="btn btn-outline btn-sm" data-hview="${v.id}">预览</button>
              <button class="btn btn-outline btn-sm" data-hrestore="${v.id}">恢复</button>
            </td>
          </tr>`;
        })
        .join("")
    : `<tr><td colspan="5" class="muted">还没有历史版本（首次修改/修复/重写后会自动留档，最多保留最近 10 版）</td></tr>`;
}

/* ---------------- 全局审校（跨章） ---------------- */

async function openGlobalReview() {
  $("#global-backdrop").hidden = false;
  $("#global-status").textContent = "";
  await refreshGlobalReview();
}

async function refreshGlobalReview() {
  const r = await api(`/api/novels/${state.novelId}/global-review`);
  $("#global-meta").textContent = r.updated_at
    ? `上次检查：${fmtTime(r.updated_at)} · 从全书视角检查跨章一致性 / 节奏 / 支线状态`
    : "还没有做过全局审校：点下方「立即全局审校」开始（建议写到 5 章以上再做）。";
  const issues = r.issues || [];
  const box = $("#global-issues");
  if (!issues.length) {
    box.innerHTML = `<p class="muted" style="margin:0;">${r.updated_at ? "最近一次检查没有发现跨章问题。" : ""}</p>`;
    return;
  }
  box.innerHTML = issues
    .map((it, i) => {
      const cls = it.severity === "high" ? "chip-danger" : it.severity === "medium" ? "chip-warning" : "";
      const st = it.status === "fixed"
        ? `<span class="chip chip-fixed">已修复</span>`
        : `<span class="chip ${cls}">${it.is_new ? "新发现" : "仍存在"}</span>`;
      const chs = (it.affected_chapters || [])
        .map((x) => `<span class="chip chip-filter" data-goto="${x}" title="打开第${x}章">第${x}章</span>`)
        .join("");
      const fixBtn = it.status !== "fixed" && (it.affected_chapters || []).length
        ? `<button class="btn btn-outline btn-sm" data-gfix="${i}">按此修复</button>`
        : "";
      return `<div class="panel" style="padding:10px 12px;margin:0 0 8px;">
        <div class="chips">${st}${chs}</div>
        <div style="font-size:13px;line-height:1.7;margin-top:6px;">${esc(it.note)}</div>
        ${it.suggestion ? `<div class="muted" style="font-size:12px;">建议：${esc(it.suggestion)}</div>` : ""}
        ${fixBtn ? `<div style="margin-top:8px;">${fixBtn}</div>` : ""}
      </div>`;
    })
    .join("");
}

function bindGlobalReview() {
  $("#novel-global-review").onclick = () => openGlobalReview().catch((e) => toast(e.message, true));
  $("#global-close").onclick = () => { $("#global-backdrop").hidden = true; };
  $("#global-backdrop").addEventListener("click", (e) => {
    if (e.target === $("#global-backdrop")) $("#global-backdrop").hidden = true;
  });
  $("#global-run").onclick = async () => {
    const btn = $("#global-run");
    btn.disabled = true;
    $("#global-status").textContent = "已开始，进度见「跟随生成」…";
    try {
      await apiSend(`/api/novels/${state.novelId}/global-review`, "POST");
      let tries = 0;
      const timer = setInterval(async () => {
        tries += 1;
        try {
          await refreshGlobalReview();
        } catch { /* 忽略 */ }
        if (tries >= 8 || $("#global-backdrop").hidden) {
          clearInterval(timer);
          $("#global-status").textContent = "";
          btn.disabled = false;
        }
      }, 3000);
    } catch (err) {
      toast(err.message, true);
      $("#global-status").textContent = "";
      btn.disabled = false;
    }
  };
  $("#global-issues").addEventListener("click", async (e) => {
    const go = e.target.closest("[data-goto]");
    if (go) {
      $("#global-backdrop").hidden = true;
      openEditor(parseInt(go.dataset.goto, 10)).catch((err) => toast(err.message, true));
      return;
    }
    const fix = e.target.closest("[data-gfix]");
    if (fix) {
      try {
        await apiSend(`/api/novels/${state.novelId}/global-review/fix`, "POST", {
          index: parseInt(fix.dataset.gfix, 10),
        });
        toast("已开始按该意见定向修复（进度见「跟随生成」）");
        setTimeout(() => refreshGlobalReview().catch(() => {}), 4000);
      } catch (err) {
        toast(err.message, true);
      }
    }
  });
}

async function openEditor(idx) {
  const seq = (state.editorSeq = (state.editorSeq || 0) + 1);
  const wantNovel = state.novelId;
  const c = await api(`/api/novels/${wantNovel}/chapters/${idx}`);
  // 请求飞行中又点了别的章节/切了作品：丢弃这次结果（避免显示错章）
  if (state.editorSeq !== seq || state.novelId !== wantNovel) return;
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
    state.editorIssues = [];
    return;
  }
  // 意见按状态分开展示：open = 仍存在（可点击再修）；fixed = AI 已修复（灰显留档）
  const open = review.issues.filter((it) => it.status !== "fixed");
  const fixed = review.issues.filter((it) => it.status === "fixed");
  state.editorIssues = open;
  const openChips = open
    .map((it, i) => {
      const cls = it.severity === "high" ? "chip-danger" : it.severity === "medium" ? "chip-warning" : "";
      const tags = [];
      if (it.is_new) tags.push("新发现");
      if (it.stale) tags.push("引用已变");
      const prefix = tags.length ? `${tags.join("·")}：` : "";
      const title = it.stale
        ? `${it.is_new ? "复核新发现。" : "仍存在。"}此意见引用的原文与当前正文对不上（稿子改过或模型引用不逐字）：点击会尽力重新定位；定位失败可先「重新审校」。${it.quote ? `引用：${it.quote}` : ""}`
        : `${it.is_new ? "复核新发现（上一轮没有这条）。" : "仍存在。"}点击可按这条意见再修一次。${it.quote ? `引用：${it.quote}` : ""}`;
      return `<span class="chip chip-filter ${cls}${it.stale ? " chip-stale" : ""}" data-fix="${i}" title="${esc(title)}">${ic("i-flag")}${esc(prefix + (it.note || it.severity))}</span>`;
    })
    .join("");
  const fixedChips = fixed
    .map((it) => {
      const note = it.note || it.severity;
      const brief = note.length > 18 ? note.slice(0, 18) + "…" : note;
      return `<span class="chip chip-fixed" title="AI 已修复（留档）：${esc(note)}">${ic("i-check")}已修复 ${esc(brief)}</span>`;
    })
    .join("");
  let head = "";
  if (open.length) head = "";
  else if (fixed.length) head = `<span class="chip chip-success">审校意见已全部修复</span>`;
  else head = `<span class="chip chip-success">审校通过</span>`;
  $("#reader-review").innerHTML = head + openChips + fixedChips;
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

function draftKey(idx, novelId = state.novelId) {
  return `ainovel.draft.${novelId}.${idx}`;
}

function closeReader() {
  $("#reader-backdrop").hidden = true;
}

let draftTimer = null;

async function saveChapter(silent = false) {
  const ta = $("#reader-content");
  const idx = parseInt(ta.dataset.idx || "0", 10);
  if (!idx) return false;
  const sent = ta.value;
  try {
    await apiSend(`/api/novels/${state.novelId}/chapters/${idx}`, "PUT", { content: sent });
    // 保存飞行期间又有新输入：保持脏标记，避免把未保存的内容当作已保存
    if (ta.value === sent) {
      state.editorDirty = false;
      $("#reader-save").disabled = true;
      try { localStorage.removeItem(draftKey(idx)); } catch { /* 忽略 */ }
    } else {
      state.editorDirty = true;
      $("#reader-save").disabled = false;
    }
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
    // 防丢稿：本地草稿（700ms 防抖）；捕获当时的作品 id，避免切书后草稿串到别的作品
    clearTimeout(draftTimer);
    const draftNovel = state.novelId;
    draftTimer = setTimeout(() => {
      const idx = parseInt(ta.dataset.idx || "0", 10);
      if (!idx) return;
      try {
        localStorage.setItem(draftKey(idx, draftNovel), JSON.stringify({ content: ta.value, at: Date.now() }));
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
  // 复制全章（含编辑器里的未保存修改），便于直接粘贴到作家平台
  const readerCopyBtn = $("#reader-copy");
  if (readerCopyBtn) {
    readerCopyBtn.onclick = async () => {
      const ta = $("#reader-content");
      const idx = parseInt(ta.dataset.idx || "0", 10) || 0;
      const titleLine = $("#reader-title").textContent.trim();
      if (!ta.value.trim()) {
        toast("本章还没有正文", true);
        return;
      }
      const ok = await copyText(`${titleLine}\n\n${ta.value.trim()}\n`);
      toast(
        ok ? `已复制第${idx}章（含未保存修改），可直接粘贴到作家平台` : "复制失败（剪贴板不可用）",
        !ok,
      );
    };
  }
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
      toast(r.review.ok ? "复核完成：通过" : `复核完成：${r.review.issues.filter((i) => i.status !== "fixed").length} 条仍存在`);
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
  $("#reader-history").onclick = () => openHistory().catch((err) => toast(err.message, true));
  $("#history-close").onclick = () => { $("#history-backdrop").hidden = true; };
  $("#history-backdrop").addEventListener("click", (e) => {
    if (e.target === $("#history-backdrop")) $("#history-backdrop").hidden = true;
  });
  $("#history-ai").onclick = async () => {
    const btn = $("#history-ai");
    btn.disabled = true;
    try {
      const r = await apiSend(`/api/novels/${state.novelId}/chapters/${historyIdx}/ai-restore`, "POST");
      toast(r.restored ? `AI 复原：${r.detail}` : `无需复原：${r.detail}`);
      if (r.restored) {
        $("#history-backdrop").hidden = true;
        await openEditor(historyIdx);
      } else {
        await refreshHistory();
      }
    } catch (err) {
      toast(err.message, true);
    } finally {
      btn.disabled = false;
    }
  };
  $("#history-rows").addEventListener("click", async (e) => {
    const view = e.target.closest("[data-hview]");
    if (view) {
      try {
        const v = await api(`/api/novels/${state.novelId}/chapters/${historyIdx}/versions/${view.dataset.hview}`);
        const pre = $("#history-preview");
        pre.hidden = false;
        pre.textContent = v.content || "";
        pre.scrollTop = 0;
      } catch (err) { toast(err.message, true); }
      return;
    }
    const restore = e.target.closest("[data-hrestore]");
    if (restore) {
      const extra = state.editorDirty ? "编辑器里有未保存的修改也会被丢弃。" : "";
      if (!confirm(`复原到该版本？当前稿会先自动留一份快照（以后还能再恢复）。${extra}`)) return;
      try {
        await apiSend(`/api/novels/${state.novelId}/chapters/${historyIdx}/versions/${restore.dataset.hrestore}/restore`, "POST");
        toast("已复原该版本");
        state.editorDirty = false;
        $("#history-backdrop").hidden = true;
        await openEditor(historyIdx);
      } catch (err) { toast(err.message, true); }
    }
  });
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
  // 保存未 trim 的原始区间：应用时要与编辑器内容逐字比对，带首尾空白也能匹配
  state.polishRange = { start, end, text: ta.value.slice(start, end) };
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
/** 在正文中定位引文：精确 → 去掉首尾引号 → 忽略空白差异 → 前缀（引文尾部被改写） */
function locateQuote(value, quote) {
  const raw = (quote || "").trim();
  if (!raw) return null;
  const cands = [
    raw,
    raw.replace(/^[「『“"'（(【\[]+/, "").replace(/[」』”"'）)】\]]+$/, ""),
  ].filter((q) => q && q.length >= 4);
  // 精确匹配
  for (const q of cands) {
    const pos = value.indexOf(q);
    if (pos >= 0) return { start: pos, end: pos + q.length, exact: true };
  }
  // 忽略空白差异 + 前缀匹配：建立「非空白字符 → 原文下标」映射
  const chars = [];
  const idx = [];
  for (let i = 0; i < value.length; i++) {
    if (!/\s/.test(value[i])) {
      chars.push(value[i]);
      idx.push(i);
    }
  }
  const vnorm = chars.join("");
  const qnorm = (s) => s.replace(/\s+/g, "");
  for (const q of cands) {
    const qn = qnorm(q);
    if (qn.length < 4) continue;
    let p = vnorm.indexOf(qn);
    let take = qn.length;
    if (p < 0) {
      for (let len = Math.min(qn.length, 16); len >= 8; len -= 2) {
        p = vnorm.indexOf(qn.slice(0, len));
        if (p >= 0) {
          take = len;
          break;
        }
      }
    }
    if (p < 0) continue;
    const start = idx[p];
    const end = idx[Math.min(p + take - 1, idx.length - 1)] + 1;
    return { start, end, exact: false };
  }
  return null;
}

/** 按一条审校意见直接修文（优先定位意见引用的原文片段；引用失效时尽力重新定位） */
async function fixIssue(issue) {
  const ta = $("#reader-content");
  let start = ta.selectionStart;
  let end = ta.selectionEnd;
  let text = ta.value.slice(start, end).trim();
  if (text.length < 8 && issue.quote) {
    const hit = locateQuote(ta.value, issue.quote);
    if (hit) {
      start = hit.start;
      end = hit.end;
      text = ta.value.slice(start, end).trim();
      ta.focus();
      ta.setSelectionRange(start, end);
      if (!hit.exact) toast("引文与原文有出入，已自动定位到最接近的段落");
    }
  }
  if (text.length < 8) {
    toast(
      issue.stale
        ? "这条意见引用的原文已对不上当前正文：可先「重新审校」刷新意见，或手动选中相关段落再修"
        : "没有定位到对应原文，请手动选中要修的段落",
      true,
    );
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
  state.polishRange = { start, end, text: ta.value.slice(start, end) };
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
    const btn = $("#form-save");
    if (btn.disabled) return; // 连点保护：插入章节/新增条目不会被提交两次
    btn.disabled = true;
    const values = {};
    $$("#form-fields [data-field]").forEach((el) => { values[el.dataset.field] = el.value; });
    try {
      await formSubmit(values);
      closeForm();
    } catch (err) {
      toast(err.message, true);
    } finally {
      btn.disabled = false;
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
  state.replaying = false;
  hideStageStatus();
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
  // 历史回放（重连时服务端重放最近 300 条）：只记日志，不做重副作用，避免重渲染风暴；
  // 回放结束后由 replay_done 统一刷新一次
  if (ev.replay) {
    state.replaying = true;
    if (ev.type === "snapshot" && ev.run) {
      novelLog(`已连接 · 当前运行：${RUN_STATUS[ev.run.status] || ev.run.status} / ${STAGE_LABEL[ev.run.stage] || ev.run.stage}`);
      if (ev.run.status === "running") {
        state.liveStage = ev.run.stage;
        state.stageSince = Date.now();
      }
    } else if (ev.type === "stage") {
      novelLog(ev.message);
    } else if (ev.type === "chapter_saved") {
      novelLog(`第${ev.chapter_idx}章已保存：${ev.title}（${ev.words} 字）`);
    }
    return;
  }
  switch (ev.type) {
    case "snapshot":
      if (ev.run) novelLog(`已连接 · 当前运行：${RUN_STATUS[ev.run.status] || ev.run.status} / ${STAGE_LABEL[ev.run.stage] || ev.run.stage}`);
      break;
    case "replay_done":
      state.replaying = false;
      refreshNovel();
      break;
    case "run_started":
      novelLog(`运行开始（目标 ${ev.chapters} 章）`);
      break;
    case "stage":
      novelLog(ev.message);
      showStageStatus(ev.stage, ev.message);
      // 审校处理等后台任务没有 run_finished：done 即收尾，停掉秒表与活跃指示
      if (ev.stage === "done") {
        $("#stream-live").classList.remove("on");
        hideStageStatus();
        refreshNovel();
      }
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
      refreshNovel();
      break;
    case "ledger_updated":
      novelLog(`资料整理：角色 ${ev.characters} 项 / 伏笔 ${ev.foreshadowings} 项`);
      if (state.tab !== "chapters") renderTab();
      break;
    case "run_paused":
      novelLog(ev.message);
      toast(ev.message || "已暂停");
      $("#stream-live").classList.remove("on");
      hideStageStatus();
      refreshNovel();
      break;
    case "run_finished":
      novelLog("本次运行完成");
      $("#stream-live").classList.remove("on");
      hideStageStatus();
      refreshNovel();
      break;
    case "run_failed":
      novelLog(`运行失败：${ev.error}`);
      $("#stream-live").classList.remove("on");
      hideStageStatus();
      refreshNovel();
      break;
  }
}

/* ---------------- 全阶段进度指示 ----------------
 * 正文生成有 token 流可跟随；立项/规划/审校/自动修复/资料整理这些
 * 「安静阶段」没有增量输出，这里用带秒表的实时状态行给用户可见反馈。
 */
function showStageStatus(stage, message) {
  const box = $("#stream-status");
  const text = $("#stream-status-text");
  if (!box || !text) return;
  state.liveStage = stage;
  state.stageSince = Date.now();
  box.hidden = false;
  text.textContent = `${STAGE_LABEL[stage] || stage} · ${message}`;
  $("#stream-live").classList.add("on");
  if (!state.stageTimer) {
    state.stageTimer = setInterval(() => {
      const el = $("#stream-status-timer");
      if (!el || !state.stageSince) return;
      const secs = Math.round((Date.now() - state.stageSince) / 1000);
      el.textContent = secs >= 2 ? `已 ${secs}s` : "";
    }, 1000);
  }
  // 步骤条按实时阶段高亮
  if (parseRoute().kind === "novel") renderNovel();
}

function hideStageStatus() {
  const box = $("#stream-status");
  if (box) box.hidden = true;
  state.stageSince = null;
  state.liveStage = null;
  if (state.stageTimer) {
    clearInterval(state.stageTimer);
    state.stageTimer = null;
  }
}

async function refreshNovel() {
  if (!state.novelId) return;
  const wantId = state.novelId;
  try {
    const data = await api(`/api/novels/${wantId}`);
    // 请求飞行中可能已切换作品：只接受仍是当前作品的响应，避免旧数据回填
    if (state.novelId !== wantId || parseRoute().kind !== "novel") return;
    state.novel = data;
    renderNovel();
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
  // 写作偏好（自动修复等）
  try {
    const p = await api("/api/settings/pipeline");
    $("#set-auto-fix").checked = !!p.auto_fix_review;
    $("#set-budget").value = p.monthly_budget_tokens > 0 ? p.monthly_budget_tokens : "";
    $("#set-auto-batch").value = p.auto_batch && p.auto_batch !== 10 ? p.auto_batch : "";
    $("#set-plan-window").value = p.plan_window && p.plan_window !== 10 ? p.plan_window : "";
    const auditEvery = typeof p.foreshadow_audit_every === "number" ? p.foreshadow_audit_every : 1;
    const sel = $("#set-audit-every");
    if (sel) {
      sel.value = String(auditEvery);
      if (sel.value !== String(auditEvery)) sel.value = "1"; // 特殊值不在选项里时回退默认
    }
    const autoFixGlobal = $("#set-auto-fix-global");
    if (autoFixGlobal) autoFixGlobal.checked = p.auto_fix_global !== false;
    const globalEvery = typeof p.global_review_every === "number" ? p.global_review_every : 10;
    const gsel = $("#set-global-every");
    if (gsel) {
      gsel.value = String(globalEvery);
      if (gsel.value !== String(globalEvery)) gsel.value = "10";
    }
  } catch {
    /* 忽略：保持默认勾选状态 */
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

  // 写作偏好：审校问题自动修复 + 月度 Token 预算 + 每批章节数（同一接口，保存时带上全部字段）
  const savePipeline = async (statusEl) => {
    const budget = Math.max(0, parseInt($("#set-budget").value || "0", 10) || 0);
    const batchRaw = parseInt($("#set-auto-batch").value || "10", 10) || 10;
    const batch = Math.min(50, Math.max(1, batchRaw));
    const planWindowRaw = parseInt($("#set-plan-window").value || "10", 10) || 10;
    const planWindow = Math.min(50, Math.max(3, planWindowRaw));
    const auditEvery = Math.min(100, Math.max(0, parseInt($("#set-audit-every").value || "1", 10) || 0));
    const globalEvery = Math.min(100, Math.max(0, parseInt($("#set-global-every").value || "10", 10) || 0));
    const autoFixGlobalEl = $("#set-auto-fix-global");
    try {
      await apiSend("/api/settings/pipeline", "PUT", {
        auto_fix_review: $("#set-auto-fix").checked,
        monthly_budget_tokens: budget,
        auto_batch: batch,
        plan_window: planWindow,
        foreshadow_audit_every: auditEvery,
        auto_fix_global: autoFixGlobalEl ? autoFixGlobalEl.checked : true,
        global_review_every: globalEvery,
      });
      const el = $("#pipeline-status");
      if (el) {
        el.textContent = statusEl || "已保存";
        setTimeout(() => { el.textContent = ""; }, 2500);
      }
    } catch (err) {
      $("#pipeline-status").textContent = "保存失败：" + err.message;
    }
  };
  $("#set-auto-fix").addEventListener("change", () =>
    savePipeline($("#set-auto-fix").checked ? "已开启自动修复" : "已关闭自动修复"),
  );
  $("#set-budget").addEventListener("change", () =>
    savePipeline($("#set-budget").value ? "预算已保存" : "预算已设为不限"),
  );
  $("#set-auto-batch").addEventListener("change", () =>
    savePipeline(`每批章节数已设为 ${Math.min(50, Math.max(1, parseInt($("#set-auto-batch").value || "10", 10) || 10))}`),
  );
  $("#set-plan-window").addEventListener("change", () =>
    savePipeline(`单次规划章数已设为 ${Math.min(50, Math.max(3, parseInt($("#set-plan-window").value || "10", 10) || 10))}`),
  );
  $("#set-audit-every").addEventListener("change", () => {
    const v = parseInt($("#set-audit-every").value, 10) || 0;
    savePipeline(v === 0 ? "已关闭例行伏笔审核（一键处理时仍会审核）" : `伏笔审核已设为：每 ${v} 章一次`);
  });
  const afg = $("#set-auto-fix-global");
  if (afg) {
    afg.addEventListener("change", () =>
      savePipeline(afg.checked ? "已开启全局审校自动修复" : "已关闭全局审校自动修复（仍可手动按意见修复）"),
    );
  }
  const ges = $("#set-global-every");
  if (ges) {
    ges.addEventListener("change", () => {
      const v = parseInt(ges.value, 10) || 0;
      savePipeline(v === 0 ? "已关闭例行全局审校（仍可手动随时跑）" : `全局审校已设为：每 ${v} 章一次`);
    });
  }
}

/* ---------------- 主题 / 引擎状态 ---------------- */

/** 主题联动原生状态栏（Android 壳注入 window.AiNovelNative；纯浏览器环境为空操作） */
function notifyNativeTheme(dark) {
  try {
    if (window.AiNovelNative && typeof window.AiNovelNative.setDark === "function") {
      window.AiNovelNative.setDark(dark);
    }
  } catch {
    /* 忽略：非安卓环境 */
  }
}

function bindTheme() {
  let saved = null;
  try {
    saved = localStorage.getItem("ainovel.theme");
  } catch {
    /* 存储不可用：跟随系统主题即可，不影响其它初始化 */
  }
  const prefersDark = window.matchMedia && window.matchMedia("(prefers-color-scheme: dark)").matches;
  const dark = saved ? saved === "dark" : prefersDark;
  document.documentElement.classList.toggle("dark", dark);
  notifyNativeTheme(dark);
  $("#theme-toggle").onclick = () => {
    const next = !document.documentElement.classList.contains("dark");
    document.documentElement.classList.toggle("dark", next);
    try {
      localStorage.setItem("ainovel.theme", next ? "dark" : "light");
    } catch {
      /* 忽略 */
    }
    notifyNativeTheme(next);
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
  bindOutline();
  bindRead();
  bindCreate();
  bindNovel();
  bindGlobalReview();
  bindBatch();
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
