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
  polishRange: null,
  polishResult: null,
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
  if (key === "active") {
    return items.filter((i) => i.run_status === "running" || i.novel.status === "writing" || i.novel.status === "await_confirm");
  }
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

function renderCreate() {
  const provider = state.settings?.provider;
  const model = provider === "mock" ? "离线演示（mock）" : state.settings?.model || "未配置";
  $("#create-hint").textContent = `当前模型：${model}（可在「模型设置」中调整）`;
}

function bindCreate() {
  $("#create-form").addEventListener("submit", async (e) => {
    e.preventDefault();
    const idea = $("#create-idea").value.trim();
    if (!idea) { toast("请先写一句灵感", true); return; }
    const chapters = Math.min(50, Math.max(1, parseInt($("#create-chapters").value || "3", 10) || 3));
    const mode = $("#create-mode").value;
    const btn = $("#create-submit");
    btn.disabled = true;
    btn.textContent = "正在创建…";
    try {
      const r = await apiSend("/api/novels", "POST", { idea, chapters, mode });
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

/* ---------------- 本书工作台 ---------------- */

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
  ].join("");
  $("#novel-meta").textContent = [n.logline, n.protagonist && `主角：${n.protagonist}`, n.style].filter(Boolean).join(" · ") || n.idea || "";

  const target = (run && run.chapters_target) || Math.max(chs.length, 3);
  if (!$("#novel-target").dataset.touched) {
    $("#novel-target").value = Math.max(chs.length, target);
  }
  $("#novel-export").href = `/api/novels/${n.id}/export`;

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
}

function renderTab() {
  $("#tab-chapters").hidden = state.tab !== "chapters";
  $("#tab-characters").hidden = state.tab !== "characters";
  $("#tab-foreshadowings").hidden = state.tab !== "foreshadowings";
  if (state.tab === "chapters") renderChapters();
  if (state.tab === "characters") renderCharacters().catch((e) => toast(e.message, true));
  if (state.tab === "foreshadowings") renderForeshadowings().catch((e) => toast(e.message, true));
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
        <td>
          ${c.status === "planned" ? `<button class="btn btn-outline btn-sm" data-plan="${c.idx}">改规划</button>` : `<button class="btn btn-outline btn-sm" data-read="${c.idx}">阅读/编辑</button>`}
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
  $("#chapter-rows").addEventListener("click", (e) => {
    const read = e.target.closest("[data-read]");
    if (read) { openEditor(parseInt(read.dataset.read, 10)).catch((err) => toast(err.message, true)); return; }
    const plan = e.target.closest("[data-plan]");
    if (plan) { editPlan(parseInt(plan.dataset.plan, 10)).catch((err) => toast(err.message, true)); }
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
  const ta = $("#reader-content");
  ta.value = c.content || "";
  ta.dataset.idx = String(idx);
  state.editorDirty = false;
  $("#reader-save").disabled = true;
  $("#reader-backdrop").hidden = false;
}

function closeReader() {
  $("#reader-backdrop").hidden = true;
}

function bindEditor() {
  const ta = $("#reader-content");
  ta.addEventListener("input", () => {
    state.editorDirty = true;
    $("#reader-save").disabled = false;
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
  $("#reader-save").onclick = async () => {
    const idx = parseInt(ta.dataset.idx || "0", 10);
    if (!idx) return;
    try {
      await apiSend(`/api/novels/${state.novelId}/chapters/${idx}`, "PUT", { content: ta.value });
      state.editorDirty = false;
      $("#reader-save").disabled = true;
      toast("已保存");
      await refreshNovel();
    } catch (err) {
      toast(err.message, true);
    }
  };
  $$("[data-polish]").forEach((btn) => {
    btn.onclick = () => runPolish(btn.dataset.polish).catch((err) => toast(err.message, true));
  });
  $("#diff-cancel").onclick = closeDiff;
  $("#diff-apply").onclick = applyPolish;
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
      syncProviderFields();
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
  bindEditor();
  bindFormDialog();
  bindSettings();
  loadSettings().catch(() => {});
  loadHealth();
  setInterval(loadHealth, 15000);
  route().catch((e) => toast(e.message, true));
}

init();
