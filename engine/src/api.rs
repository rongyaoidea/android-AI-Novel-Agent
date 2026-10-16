use std::convert::Infallible;
use std::sync::Arc;

use axum::extract::{DefaultBodyLimit, Path, Query, Request, State};
use axum::http::{header, StatusCode};
use axum::middleware::{self, Next};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::json;

use crate::llm::record_usage;
use crate::models::LlmSettings;
use crate::pipeline;
use crate::Engine;

type ApiErr = (StatusCode, String);

fn err(e: impl std::fmt::Display) -> ApiErr {
    (StatusCode::INTERNAL_SERVER_ERROR, format!("{e}"))
}

fn not_found(what: &str) -> ApiErr {
    (StatusCode::NOT_FOUND, format!("{what} not found"))
}

// ---------- 本地访问令牌 ----------

/// 常量时间比较（避免时序侧信道）
fn constant_time_eq(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

/// 令牌校验：`/api/*` 需要 `x-ainovel-token` 头、`Authorization: Bearer` 或 `?token=`
/// （EventSource 与下载链接无法加请求头，只能走查询参数）。
/// `/api/health` 与静态资源放行（就绪探测、页面加载）。
/// 未配置令牌（本地开发）时不设防。
async fn auth_middleware(State(e): State<Arc<Engine>>, req: Request, next: Next) -> Response {
    let Some(expected) = e.auth_token() else {
        return next.run(req).await;
    };
    let path = req.uri().path().to_string();
    if !path.starts_with("/api/") || path == "/api/health" {
        return next.run(req).await;
    }
    let header_ok = req
        .headers()
        .get("x-ainovel-token")
        .and_then(|v| v.to_str().ok())
        .map(|t| constant_time_eq(t, expected))
        .unwrap_or(false)
        || req
            .headers()
            .get(header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "))
            .map(|t| constant_time_eq(t, expected))
            .unwrap_or(false);
    let query_ok = req
        .uri()
        .query()
        .map(|q| {
            q.split('&').any(|kv| {
                let mut it = kv.splitn(2, '=');
                it.next() == Some("token")
                    && it
                        .next()
                        .map(|v| constant_time_eq(v, expected))
                        .unwrap_or(false)
            })
        })
        .unwrap_or(false);
    if header_ok || query_ok {
        next.run(req).await
    } else {
        (
            StatusCode::UNAUTHORIZED,
            "缺少或错误的访问令牌（x-ainovel-token / ?token=）".to_string(),
        )
            .into_response()
    }
}

pub fn build_router(engine: Arc<Engine>) -> Router {
    Router::new()
        .route("/", get(index))
        .route("/app.css", get(app_css))
        .route("/app.js", get(app_js))
        .route("/api/health", get(health))
        .route("/api/styles", get(list_styles))
        .route("/api/prompts", get(list_prompts))
        .route(
            "/api/prompts/:task",
            axum::routing::put(put_prompt).delete(delete_prompt),
        )
        .route("/api/settings/llm", get(get_llm).put(put_llm))
        .route("/api/settings/llm/test", post(test_llm))
        .route("/api/settings/llm/models", post(list_llm_models))
        .route("/api/novels", get(list_novels).post(create_novel))
        .route("/api/novels/:id", get(get_novel).patch(patch_novel).delete(delete_novel))
        .route("/api/novels/:id/chapters", post(insert_chapter))
        .route(
            "/api/novels/:id/chapters/:idx",
            get(get_chapter).put(edit_chapter).delete(delete_chapter),
        )
        .route("/api/novels/:id/chapters/:idx/plan", axum::routing::put(edit_plan))
        .route("/api/novels/:id/chapters/:idx/polish", post(polish_chapter))
        .route("/api/novels/:id/chapters/:idx/review", post(review_chapter))
        .route("/api/novels/:id/chapters/:idx/move", axum::routing::put(move_chapter))
        .route("/api/novels/:id/chapters/:idx/continue", post(continue_chapter))
        .route("/api/novels/:id/chapters/:idx/rewrite", post(rewrite_chapter))
        .route("/api/novels/:id/chapters/:idx/extract", post(extract_chapter))
        .route("/api/novels/:id/health", get(novel_health))
        .route("/api/novels/:id/run", post(run_novel))
        .route("/api/novels/:id/events", get(events))
        .route("/api/novels/:id/export", get(export_novel))
        .route("/api/novels/:id/characters", get(list_characters).post(add_character))
        .route(
            "/api/novels/:id/characters/:cid",
            axum::routing::put(update_character).delete(delete_character),
        )
        .route(
            "/api/novels/:id/foreshadowings",
            get(list_foreshadowings).post(add_foreshadowing),
        )
        .route(
            "/api/novels/:id/foreshadowings/:fid",
            axum::routing::put(update_foreshadowing).delete(delete_foreshadowing),
        )
        .route("/api/novels/:id/usage", get(usage_summary))
        .route("/api/runs", get(list_runs))
        .route("/api/backup", get(backup_db))
        .route(
            "/api/restore",
            post(restore_db).layer(DefaultBodyLimit::max(256 * 1024 * 1024)),
        )
        .with_state(engine.clone())
        .layer(middleware::from_fn_with_state(engine, auth_middleware))
}

// ---------- 静态资源 ----------

async fn index() -> impl IntoResponse {
    (
        [
            (header::CONTENT_TYPE, "text/html; charset=utf-8"),
            (header::CACHE_CONTROL, "no-store"),
        ],
        crate::webui::INDEX_HTML,
    )
}

async fn app_css() -> impl IntoResponse {
    (
        [
            (header::CONTENT_TYPE, "text/css; charset=utf-8"),
            (header::CACHE_CONTROL, "no-store"),
        ],
        crate::webui::APP_CSS,
    )
}

async fn app_js() -> impl IntoResponse {
    (
        [
            (header::CONTENT_TYPE, "application/javascript; charset=utf-8"),
            (header::CACHE_CONTROL, "no-store"),
        ],
        crate::webui::APP_JS,
    )
}

// ---------- 基础 ----------

async fn health() -> Json<serde_json::Value> {
    Json(json!({"ok": true, "name": "ainovel-engine", "version": env!("CARGO_PKG_VERSION")}))
}

/// 文风预设目录（UI 选择器数据源；含规则/反例，便于前端展示与自检）
async fn list_styles() -> Json<serde_json::Value> {
    Json(json!({"items": crate::styles::PRESETS}))
}

// ---------- 提示词模板 ----------

/// 任务目录 + 默认模板全文 + 当前覆盖（供前端编辑 / 预览 / 恢复默认）
async fn list_prompts(State(e): State<Arc<Engine>>) -> Result<Json<serde_json::Value>, ApiErr> {
    let ov = e.prompt_overrides();
    let items: Vec<serde_json::Value> = crate::prompts::TASKS
        .iter()
        .map(|t| {
            json!({
                "id": t.id,
                "label": t.label,
                "desc": t.desc,
                "vars": t.vars,
                "default_system": t.default_system,
                "default_user": t.default_user,
                "custom": ov.get(t.id),
            })
        })
        .collect();
    Ok(Json(json!({"items": items})))
}

#[derive(Deserialize)]
struct PromptBody {
    system: String,
    user: String,
}

/// 保存某任务的模板覆盖（校验变量名；非法 400）
async fn put_prompt(
    State(e): State<Arc<Engine>>,
    Path(task_id): Path<String>,
    Json(b): Json<PromptBody>,
) -> Result<Json<serde_json::Value>, ApiErr> {
    let spec = crate::prompts::task(&task_id)
        .ok_or_else(|| (StatusCode::NOT_FOUND, format!("未知任务: {task_id}")))?;
    let tpl = crate::prompts::validate(spec, &b.system, &b.user)
        .map_err(|msg| (StatusCode::BAD_REQUEST, msg))?;
    let mut ov = e.prompt_overrides();
    ov.set(spec.id, tpl);
    e.db.set_prompt_overrides_json(&ov.to_json_string())
        .map_err(err)?;
    Ok(Json(json!({"ok": true})))
}

/// 恢复某任务的默认模板
async fn delete_prompt(
    State(e): State<Arc<Engine>>,
    Path(task_id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiErr> {
    let spec = crate::prompts::task(&task_id)
        .ok_or_else(|| (StatusCode::NOT_FOUND, format!("未知任务: {task_id}")))?;
    let mut ov = e.prompt_overrides();
    ov.remove(spec.id);
    e.db.set_prompt_overrides_json(&ov.to_json_string())
        .map_err(err)?;
    Ok(Json(json!({"ok": true})))
}

async fn get_llm(State(e): State<Arc<Engine>>) -> Result<Json<serde_json::Value>, ApiErr> {
    let s = e.llm_settings().map_err(err)?;
    Ok(Json(serde_json::to_value(s).map_err(err)?))
}

async fn put_llm(
    State(e): State<Arc<Engine>>,
    Json(s): Json<LlmSettings>,
) -> Result<Json<serde_json::Value>, ApiErr> {
    e.save_llm_settings(&s).map_err(err)?;
    Ok(Json(json!({"ok": true})))
}

async fn test_llm(State(e): State<Arc<Engine>>) -> Result<Json<serde_json::Value>, ApiErr> {
    let settings = e.llm_settings().map_err(err)?;
    let llm = crate::llm::Llm::new(settings.clone());
    match llm.complete("你是一个连通性测试助手。", "只回复两个字：正常").await {
        Ok(out) => {
            record_usage(&e.db, None, None, "test", &settings, &out.usage);
            Ok(Json(json!({
                "ok": true,
                "message": out.text.chars().take(80).collect::<String>(),
            })))
        }
        Err(e2) => Ok(Json(json!({"ok": false, "message": format!("{e2}")}))),
    }
}

#[derive(Deserialize)]
struct ModelsBody {
    /// 表单里正在填写的 Base URL（为空则用已保存的设置）
    #[serde(default)]
    base_url: String,
    /// 表单里正在填写的 API Key（为空则用已保存的 Key）
    #[serde(default)]
    api_key: String,
}

/// 拉取服务商模型列表（OpenAI 兼容 GET /models），供设置页下拉选择
async fn list_llm_models(
    State(e): State<Arc<Engine>>,
    Json(b): Json<ModelsBody>,
) -> Result<Json<serde_json::Value>, ApiErr> {
    let stored = e.llm_settings().map_err(err)?;
    let base_url = if b.base_url.trim().is_empty() {
        stored.base_url.clone()
    } else {
        b.base_url.trim().to_string()
    };
    let api_key = if b.api_key.trim().is_empty() {
        stored.api_key.clone()
    } else {
        b.api_key.trim().to_string()
    };
    if base_url.trim().is_empty() {
        return Err((StatusCode::BAD_REQUEST, "请先填写 Base URL".to_string()));
    }
    let llm = crate::llm::Llm::new(LlmSettings {
        provider: "openai".into(),
        base_url,
        api_key,
        model: String::new(),
        temperature: 0.0,
    });
    match llm.list_models().await {
        Ok(models) => Ok(Json(json!({"ok": true, "models": models}))),
        Err(e2) => Ok(Json(json!({"ok": false, "message": format!("{e2}")}))),
    }
}

// ---------- 小说 ----------

#[derive(Deserialize)]
struct CreateNovelBody {
    idea: String,
    #[serde(default = "default_chapters")]
    chapters: i64,
    #[serde(default)]
    mode: Option<String>,
    /// 文风预设 id（可选；非法值自动忽略）
    #[serde(default)]
    style_preset: Option<String>,
}

fn default_chapters() -> i64 {
    3
}

async fn create_novel(
    State(e): State<Arc<Engine>>,
    Json(b): Json<CreateNovelBody>,
) -> Result<Json<serde_json::Value>, ApiErr> {
    let idea = b.idea.trim();
    if idea.is_empty() {
        return Err((StatusCode::BAD_REQUEST, "idea 不能为空".to_string()));
    }
    let mode = b.mode.as_deref().unwrap_or("auto");
    let style = b
        .style_preset
        .as_deref()
        .and_then(crate::styles::find)
        .map(|p| p.id)
        .unwrap_or("");
    let novel = e.db.create_novel(idea, mode, style).map_err(err)?;
    let run_id = pipeline::spawn(e.clone(), &novel.id, b.chapters.clamp(1, 50)).map_err(err)?;
    Ok(Json(json!({"novel_id": novel.id, "run_id": run_id})))
}

async fn list_novels(State(e): State<Arc<Engine>>) -> Result<Json<serde_json::Value>, ApiErr> {
    let items = e.db.list_novels().map_err(err)?;
    Ok(Json(json!({"items": items})))
}

async fn get_novel(
    State(e): State<Arc<Engine>>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiErr> {
    let novel = e
        .db
        .get_novel(&id)
        .map_err(err)?
        .ok_or_else(|| not_found("novel"))?;
    let chapters = e.db.list_chapters(&id).map_err(err)?;
    let run = e.db.latest_run(&id).map_err(err)?;
    let chapters_json: Vec<serde_json::Value> = chapters
        .into_iter()
        .map(|c| {
            json!({
                "idx": c.idx,
                "title": c.title,
                "goal": c.goal,
                "beats": c.beats,
                "status": c.status,
                "words": c.content.chars().count(),
                "preview": c.content.chars().take(60).collect::<String>(),
                "review": c.review_json,
            })
        })
        .collect();
    Ok(Json(json!({"novel": novel, "chapters": chapters_json, "run": run})))
}

#[derive(Deserialize)]
struct PatchNovelBody {
    #[serde(default)]
    mode: Option<String>,
    /// 文风预设 id（空串 = 不限；非法值返回 400）
    #[serde(default)]
    style_preset: Option<String>,
}

async fn patch_novel(
    State(e): State<Arc<Engine>>,
    Path(id): Path<String>,
    Json(b): Json<PatchNovelBody>,
) -> Result<Json<serde_json::Value>, ApiErr> {
    if e.db.get_novel(&id).map_err(err)?.is_none() {
        return Err(not_found("novel"));
    }
    if let Some(mode) = b.mode.as_deref() {
        let m = if mode == "staged" { "staged" } else { "auto" };
        e.db.set_novel_mode(&id, m).map_err(err)?;
    }
    if let Some(style) = b.style_preset.as_deref() {
        let trimmed = style.trim();
        let normalized = if trimmed.is_empty() {
            ""
        } else {
            crate::styles::find(trimmed).map(|p| p.id).ok_or_else(|| {
                (StatusCode::BAD_REQUEST, format!("未知的文风预设: {trimmed}"))
            })?
        };
        e.db.set_novel_style(&id, normalized).map_err(err)?;
    }
    Ok(Json(json!({"ok": true})))
}

async fn delete_novel(
    State(e): State<Arc<Engine>>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiErr> {
    let ok = e.db.delete_novel(&id).map_err(err)?;
    if !ok {
        return Err(not_found("novel"));
    }
    Ok(Json(json!({"ok": true})))
}

// ---------- 章节 ----------

async fn get_chapter(
    State(e): State<Arc<Engine>>,
    Path((id, idx)): Path<(String, i64)>,
) -> Result<Json<serde_json::Value>, ApiErr> {
    let ch = e
        .db
        .get_chapter(&id, idx)
        .map_err(err)?
        .ok_or_else(|| not_found("chapter"))?;
    Ok(Json(json!({
        "idx": ch.idx,
        "title": ch.title,
        "goal": ch.goal,
        "beats": ch.beats,
        "content": ch.content,
        "summary": ch.summary,
        "words": ch.content.chars().count(),
        "status": ch.status,
        "review": ch.review_json,
        "style_report": serde_json::from_str::<serde_json::Value>(&ch.style_report).ok(),
    })))
}

#[derive(Deserialize)]
struct EditChapterBody {
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    title: Option<String>,
}

/// 手动编辑正文 / 标题；写入正文即视为定稿（流水线不再覆盖）。
async fn edit_chapter(
    State(e): State<Arc<Engine>>,
    Path((id, idx)): Path<(String, i64)>,
    Json(b): Json<EditChapterBody>,
) -> Result<Json<serde_json::Value>, ApiErr> {
    if b.content.is_none() && b.title.is_none() {
        return Err((StatusCode::BAD_REQUEST, "需要提供 content 或 title".to_string()));
    }
    let ch = e
        .db
        .get_chapter(&id, idx)
        .map_err(err)?
        .ok_or_else(|| not_found("chapter"))?;
    let running = matches!(e.db.latest_run(&id).map_err(err)?, Some(r) if r.status == "running");
    if running && ch.status != "done" {
        return Err((
            StatusCode::CONFLICT,
            "本章正在生成中，完成后即可编辑。".to_string(),
        ));
    }
    let title = b
        .title
        .as_ref()
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty());
    e.db.save_chapter_edit(&id, idx, b.content.as_deref(), title.as_deref())
        .map_err(err)?;
    Ok(Json(json!({"ok": true})))
}

#[derive(Deserialize)]
struct PlanBody {
    title: String,
    #[serde(default)]
    goal: String,
    #[serde(default)]
    beats: Vec<String>,
}

/// 编辑章节规划（仅限尚未开写的章节）——供"按阶段确认"时人工调整章纲。
async fn edit_plan(
    State(e): State<Arc<Engine>>,
    Path((id, idx)): Path<(String, i64)>,
    Json(b): Json<PlanBody>,
) -> Result<Json<serde_json::Value>, ApiErr> {
    let title = b.title.trim();
    if title.is_empty() {
        return Err((StatusCode::BAD_REQUEST, "标题不能为空".to_string()));
    }
    let ch = e
        .db
        .get_chapter(&id, idx)
        .map_err(err)?
        .ok_or_else(|| not_found("chapter"))?;
    if ch.status != "planned" {
        return Err((
            StatusCode::CONFLICT,
            "已开写的章节不能修改规划（标题可单独编辑）。".to_string(),
        ));
    }
    e.db.update_chapter_plan(&id, idx, title, b.goal.trim(), &b.beats)
        .map_err(err)?;
    Ok(Json(json!({"ok": true})))
}

#[derive(Deserialize)]
struct PolishBody {
    mode: String,
    selection: String,
    #[serde(default)]
    instruction: String,
}

/// 选段修文：rewrite / expand / shrink / depolish。只返回修改稿，由前端确认后自行保存。
async fn polish_chapter(
    State(e): State<Arc<Engine>>,
    Path((id, idx)): Path<(String, i64)>,
    Json(b): Json<PolishBody>,
) -> Result<Json<serde_json::Value>, ApiErr> {
    let ch = e
        .db
        .get_chapter(&id, idx)
        .map_err(err)?
        .ok_or_else(|| not_found("chapter"))?;
    let sel = b.selection.trim();
    let len = sel.chars().count();
    if len < 8 {
        return Err((StatusCode::BAD_REQUEST, "请先选中至少 8 个字的内容".to_string()));
    }
    if len > 4000 {
        return Err((StatusCode::BAD_REQUEST, "选中内容过长（>4000 字），请分段精修".to_string()));
    }
    let mode = match b.mode.as_str() {
        "expand" | "shrink" | "depolish" | "rewrite" => b.mode.clone(),
        _ => "rewrite".to_string(),
    };
    let settings = e.llm_settings().map_err(err)?;
    let llm = crate::llm::Llm::new(settings.clone());
    let style = e
        .db
        .get_novel(&id)
        .map_err(err)?
        .and_then(|n| crate::styles::find(&n.style_preset));
    let (sys, usr) = crate::prompts::polish_prompt(&mode, sel, &ch.title, &b.instruction, style, &e.prompt_overrides());
    let out = llm.complete(&sys, &usr).await.map_err(err)?;
    record_usage(&e.db, Some(&id), Some(idx), "polish", &settings, &out.usage);
    Ok(Json(json!({"revised": out.text.trim(), "mode": mode})))
}

/// 结构编辑类操作的保护：运行中不允许改章节结构
fn ensure_not_running(e: &Engine, novel_id: &str) -> Result<(), ApiErr> {
    let running = matches!(e.db.latest_run(novel_id).map_err(err)?, Some(r) if r.status == "running");
    if running {
        return Err((
            StatusCode::CONFLICT,
            "有运行进行中，请完成后再调整章节结构。".to_string(),
        ));
    }
    Ok(())
}

#[derive(Deserialize)]
struct InsertChapterBody {
    #[serde(default)]
    after_idx: i64,
    #[serde(default)]
    title: String,
    #[serde(default)]
    goal: String,
}

/// 插入章节（after_idx=0 表示插到最前）
async fn insert_chapter(
    State(e): State<Arc<Engine>>,
    Path(id): Path<String>,
    Json(b): Json<InsertChapterBody>,
) -> Result<Json<serde_json::Value>, ApiErr> {
    if e.db.get_novel(&id).map_err(err)?.is_none() {
        return Err(not_found("novel"));
    }
    ensure_not_running(&e, &id)?;
    let title = if b.title.trim().is_empty() {
        "新章节".to_string()
    } else {
        b.title.trim().to_string()
    };
    let idx = e
        .db
        .insert_chapter_after(&id, b.after_idx, &title, b.goal.trim())
        .map_err(err)?;
    Ok(Json(json!({"ok": true, "idx": idx})))
}

/// 删除章节并重排（至少保留一章）
async fn delete_chapter(
    State(e): State<Arc<Engine>>,
    Path((id, idx)): Path<(String, i64)>,
) -> Result<Json<serde_json::Value>, ApiErr> {
    if e.db.get_chapter(&id, idx).map_err(err)?.is_none() {
        return Err(not_found("chapter"));
    }
    ensure_not_running(&e, &id)?;
    let ok = e.db.delete_chapter_at(&id, idx).map_err(err)?;
    if !ok {
        return Err((StatusCode::CONFLICT, "至少保留一章，无法继续删除。".to_string()));
    }
    Ok(Json(json!({"ok": true})))
}

#[derive(Deserialize)]
struct MoveChapterBody {
    direction: String,
}

/// 章节上移 / 下移
async fn move_chapter(
    State(e): State<Arc<Engine>>,
    Path((id, idx)): Path<(String, i64)>,
    Json(b): Json<MoveChapterBody>,
) -> Result<Json<serde_json::Value>, ApiErr> {
    if e.db.get_chapter(&id, idx).map_err(err)?.is_none() {
        return Err(not_found("chapter"));
    }
    ensure_not_running(&e, &id)?;
    let direction = match b.direction.as_str() {
        "up" => -1,
        "down" => 1,
        _ => return Err((StatusCode::BAD_REQUEST, "direction 只能是 up 或 down".to_string())),
    };
    let moved = e.db.move_chapter(&id, idx, direction).map_err(err)?;
    if !moved {
        return Err((StatusCode::CONFLICT, "已经到边界了。".to_string()));
    }
    Ok(Json(json!({"ok": true})))
}

/// 单章重新审校（保存/修文后复核用）
async fn review_chapter(
    State(e): State<Arc<Engine>>,
    Path((id, idx)): Path<(String, i64)>,
) -> Result<Json<serde_json::Value>, ApiErr> {
    let ch = e
        .db
        .get_chapter(&id, idx)
        .map_err(err)?
        .ok_or_else(|| not_found("chapter"))?;
    if ch.content.trim().is_empty() {
        return Err((StatusCode::BAD_REQUEST, "本章还没有正文，无法复核。".to_string()));
    }
    let settings = e.llm_settings().map_err(err)?;
    let llm = crate::llm::Llm::new(settings.clone());
    let style = e
        .db
        .get_novel(&id)
        .map_err(err)?
        .and_then(|n| crate::styles::find(&n.style_preset));
    let (sys, usr) = crate::prompts::review_prompt(&ch.content, style, &e.prompt_overrides());
    let out = llm.complete(&sys, &usr).await.map_err(err)?;
    record_usage(&e.db, Some(&id), Some(idx), "review", &settings, &out.usage);
    let raw = out.text;
    let v = crate::llm::extract_json_object(&raw).ok_or_else(|| {
        (StatusCode::INTERNAL_SERVER_ERROR, "复核输出不是 JSON".to_string())
    })?;
    let review = crate::models::Review {
        ok: v["ok"].as_bool().unwrap_or(true),
        issues: v["issues"]
            .as_array()
            .map(|a| {
                a.iter()
                    .map(|it| crate::models::ReviewIssue {
                        severity: it["severity"].as_str().unwrap_or("info").to_string(),
                        note: it["note"].as_str().unwrap_or("").to_string(),
                        quote: it["quote"].as_str().unwrap_or("").to_string(),
                    })
                    .collect()
            })
            .unwrap_or_default(),
    };
    e.db.save_chapter_review(&id, idx, &review).map_err(err)?;
    e.hub.publish(&id, pipeline::EngineEvent::ChapterReview {
        chapter_idx: idx,
        ok: review.ok,
        issues: review.issues.len(),
    });
    Ok(Json(json!({"ok": true, "review": review})))
}

#[derive(Deserialize)]
struct ContinueBody {
    before: String,
    #[serde(default = "default_continue_chars")]
    chars: i64,
    #[serde(default)]
    instruction: String,
}

fn default_continue_chars() -> i64 {
    600
}

/// 光标续写：把光标前文交给模型接着写（只返回文本，由编辑器插入）
async fn continue_chapter(
    State(e): State<Arc<Engine>>,
    Path((id, idx)): Path<(String, i64)>,
    Json(b): Json<ContinueBody>,
) -> Result<Json<serde_json::Value>, ApiErr> {
    let ch = e
        .db
        .get_chapter(&id, idx)
        .map_err(err)?
        .ok_or_else(|| not_found("chapter"))?;
    let chars = b.chars.clamp(100, 2000);
    let text = b.before.trim();
    if text.is_empty() {
        return Err((StatusCode::BAD_REQUEST, "光标前面还没有内容，无法续写".to_string()));
    }
    let before: String = {
        let n = text.chars().count();
        if n > 1500 {
            text.chars().skip(n - 1500).collect()
        } else {
            text.to_string()
        }
    };
    let settings = e.llm_settings().map_err(err)?;
    let llm = crate::llm::Llm::new(settings.clone());
    let style = e
        .db
        .get_novel(&id)
        .map_err(err)?
        .and_then(|n| crate::styles::find(&n.style_preset));
    let (sys, usr) = crate::prompts::continue_prompt(&ch.title, &before, chars, &b.instruction, style, &e.prompt_overrides());
    let out = llm.complete(&sys, &usr).await.map_err(err)?;
    record_usage(&e.db, Some(&id), Some(idx), "continue", &settings, &out.usage);
    Ok(Json(json!({"text": out.text.trim(), "chars": chars})))
}

#[derive(Deserialize)]
struct RewriteBody {
    #[serde(default)]
    mode: String,
    #[serde(default)]
    instruction: String,
}

/// 章节重写：regenerate=按章纲重新生成整章；revise=按意见改写现有正文（只返回新稿）
async fn rewrite_chapter(
    State(e): State<Arc<Engine>>,
    Path((id, idx)): Path<(String, i64)>,
    Json(b): Json<RewriteBody>,
) -> Result<Json<serde_json::Value>, ApiErr> {
    let ch = e
        .db
        .get_chapter(&id, idx)
        .map_err(err)?
        .ok_or_else(|| not_found("chapter"))?;
    let mode = match b.mode.as_str() {
        "regenerate" => "regenerate",
        "revise" => "revise",
        _ => {
            if ch.content.trim().is_empty() {
                "regenerate"
            } else {
                "revise"
            }
        }
    };
    if mode == "revise" && ch.content.trim().is_empty() {
        return Err((StatusCode::BAD_REQUEST, "本章还没有正文，请选择「重新生成」".to_string()));
    }
    let settings = e.llm_settings().map_err(err)?;
    let llm = crate::llm::Llm::new(settings.clone());
    let style = e
        .db
        .get_novel(&id)
        .map_err(err)?
        .and_then(|n| crate::styles::find(&n.style_preset));
    let (sys, usr) = crate::prompts::rewrite_prompt(
        mode,
        &ch.title,
        &ch.goal,
        &ch.content,
        &b.instruction,
        style,
        &e.prompt_overrides(),
    );
    let out = llm.complete(&sys, &usr).await.map_err(err)?;
    record_usage(&e.db, Some(&id), Some(idx), "rewrite", &settings, &out.usage);
    Ok(Json(json!({"text": out.text.trim(), "mode": mode})))
}

/// 单章资料整理（补摘要 / 补台账）；书籍体检的「补齐资料」逐章调用
async fn extract_chapter(
    State(e): State<Arc<Engine>>,
    Path((id, idx)): Path<(String, i64)>,
) -> Result<Json<serde_json::Value>, ApiErr> {
    let ch = e
        .db
        .get_chapter(&id, idx)
        .map_err(err)?
        .ok_or_else(|| not_found("chapter"))?;
    if ch.content.trim().is_empty() {
        return Err((StatusCode::BAD_REQUEST, "本章还没有正文，无法整理资料".to_string()));
    }
    let settings = e.llm_settings().map_err(err)?;
    let llm = crate::llm::Llm::new(settings.clone());
    let (sys, usr) = crate::prompts::extract_prompt(idx, &ch.content, &e.prompt_overrides());
    let out = llm.complete(&sys, &usr).await.map_err(err)?;
    record_usage(&e.db, Some(&id), Some(idx), "extract", &settings, &out.usage);
    let raw = out.text;
    let j = crate::llm::extract_json_object(&raw).ok_or_else(|| {
        (StatusCode::INTERNAL_SERVER_ERROR, "整理输出不是 JSON".to_string())
    })?;
    let (chars, fores) = pipeline::apply_extraction(&e.db, &id, idx, &j);
    e.hub.publish(&id, pipeline::EngineEvent::LedgerUpdated {
        chapter_idx: idx,
        characters: chars,
        foreshadowings: fores,
    });
    let summary = j["summary"].as_str().unwrap_or("").trim().to_string();
    Ok(Json(json!({
        "ok": true,
        "characters": chars,
        "foreshadowings": fores,
        "summary": summary,
    })))
}

fn join_idx(list: &[i64]) -> String {
    let head: Vec<String> = list.iter().take(8).map(|i| format!("第{i}章")).collect();
    if list.len() > 8 {
        format!("{} 等", head.join("、"))
    } else {
        head.join("、")
    }
}

/// 书籍体检：结构 / 摘要 / 审校 / 伏笔 / 运行状态汇总 + 行动建议
async fn novel_health(
    State(e): State<Arc<Engine>>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiErr> {
    let novel = e
        .db
        .get_novel(&id)
        .map_err(err)?
        .ok_or_else(|| not_found("novel"))?;
    let chapters = e.db.list_chapters(&id).map_err(err)?;
    let characters = e.db.list_characters(&id).map_err(err)?;
    let fores = e.db.list_foreshadowings(&id).map_err(err)?;
    let run = e.db.latest_run(&id).map_err(err)?;

    let total = chapters.len() as i64;
    let done = chapters
        .iter()
        .filter(|c| c.status == "done" && !c.content.trim().is_empty())
        .count() as i64;
    let missing_content: Vec<i64> = chapters
        .iter()
        .filter(|c| c.content.trim().is_empty())
        .map(|c| c.idx)
        .collect();
    let missing_summary: Vec<i64> = chapters
        .iter()
        .filter(|c| !c.content.trim().is_empty() && c.summary.trim().is_empty())
        .map(|c| c.idx)
        .collect();

    let mut review_warnings = 0usize;
    let mut review_chapters: Vec<i64> = Vec::new();
    for c in &chapters {
        if let Some(raw) = c.review_json.as_deref() {
            if let Ok(r) = serde_json::from_str::<crate::models::Review>(raw) {
                if !r.ok || !r.issues.is_empty() {
                    review_warnings += r.issues.len().max(1);
                    review_chapters.push(c.idx);
                }
            }
        }
    }

    let open: Vec<&crate::models::Foreshadowing> = fores.iter().filter(|f| f.status == "open").collect();
    let oldest_open = open
        .iter()
        .filter(|f| f.planted_chapter > 0)
        .map(|f| f.planted_chapter)
        .min();

    let mut suggestions: Vec<String> = Vec::new();
    if !missing_content.is_empty() {
        suggestions.push(format!(
            "还有 {} 章尚未完成正文：{}",
            missing_content.len(),
            join_idx(&missing_content)
        ));
    }
    if !missing_summary.is_empty() {
        suggestions.push(format!(
            "有 {} 章缺摘要，可点「补齐资料」自动生成：{}",
            missing_summary.len(),
            join_idx(&missing_summary)
        ));
    }
    if review_warnings > 0 {
        suggestions.push(format!(
            "有 {} 章共 {} 条审校意见待处理：{}",
            review_chapters.len(),
            review_warnings,
            join_idx(&review_chapters)
        ));
    }
    if let Some(oldest) = oldest_open {
        if total > 0 && oldest + 5 <= total {
            suggestions.push(format!(
                "最早埋于第 {} 章的伏笔已经 {} 章未回收，建议安排回收",
                oldest,
                total - oldest
            ));
        }
    }
    if let Some(r) = &run {
        if r.status == "failed" {
            suggestions.push(format!("上次运行中断：{}", r.error.clone().unwrap_or_default()));
        }
        if r.status == "paused" {
            suggestions.push("有运行在等待确认（章节规划），到工作台点「确认并继续」".to_string());
        }
    }
    if suggestions.is_empty() {
        suggestions.push("没有发现明显风险，继续推进即可。".to_string());
    }

    Ok(Json(json!({
        "novel_id": novel.id,
        "mode": novel.mode,
        "chapters_total": total,
        "chapters_done": done,
        "missing_content": missing_content,
        "missing_summary": missing_summary,
        "review_warnings": review_warnings,
        "review_chapters": review_chapters,
        "characters": characters.len(),
        "open_foreshadowings": open.len(),
        "oldest_open_chapter": oldest_open,
        "last_run_status": run.as_ref().map(|r| r.status.clone()),
        "suggestions": suggestions,
    })))
}

// ---------- 创作运行 ----------

#[derive(Deserialize)]
struct RunBody {
    #[serde(default = "default_chapters")]
    chapters: i64,
}

async fn run_novel(
    State(e): State<Arc<Engine>>,
    Path(id): Path<String>,
    Json(b): Json<RunBody>,
) -> Result<Json<serde_json::Value>, ApiErr> {
    if e.db.get_novel(&id).map_err(err)?.is_none() {
        return Err(not_found("novel"));
    }
    if let Some(run) = e.db.latest_run(&id).map_err(err)? {
        if run.status == "running" {
            return Err((
                StatusCode::CONFLICT,
                "已有运行进行中，请等待当前任务完成后再继续。".to_string(),
            ));
        }
    }
    let run_id = pipeline::spawn(e.clone(), &id, b.chapters.clamp(1, 50)).map_err(err)?;
    Ok(Json(json!({"run_id": run_id})))
}

async fn events(
    State(e): State<Arc<Engine>>,
    Path(id): Path<String>,
) -> Sse<impl futures_util::Stream<Item = Result<Event, Infallible>>> {
    let mut rx = e.hub.subscribe();
    let snapshot = e.db.latest_run(&id).ok().flatten();
    // 重连回放：本运行的关键事件（Token 不入缓冲）
    let history = e.hub.history(&id);
    let stream = async_stream::stream! {
        if let Some(run) = snapshot {
            let payload = json!({"type": "snapshot", "run": run}).to_string();
            yield Ok(Event::default().event("engine").data(payload));
        }
        for ev in history {
            if let Ok(s) = serde_json::to_string(&ev) {
                yield Ok(Event::default().event("engine").data(s));
            }
        }
        loop {
            match rx.recv().await {
                Ok(ev) => {
                    if let Ok(s) = serde_json::to_string(&ev) {
                        yield Ok(Event::default().event("engine").data(s));
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            }
        }
    };
    Sse::new(stream).keep_alive(KeepAlive::default())
}

// ---------- 角色台账 ----------

async fn list_characters(
    State(e): State<Arc<Engine>>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiErr> {
    let items = e.db.list_characters(&id).map_err(err)?;
    Ok(Json(json!({"items": items})))
}

#[derive(Deserialize)]
struct CharacterBody {
    name: String,
    #[serde(default)]
    role: String,
}

async fn add_character(
    State(e): State<Arc<Engine>>,
    Path(id): Path<String>,
    Json(b): Json<CharacterBody>,
) -> Result<Json<serde_json::Value>, ApiErr> {
    let name = b.name.trim();
    if name.is_empty() {
        return Err((StatusCode::BAD_REQUEST, "角色名不能为空".to_string()));
    }
    if e.db.get_novel(&id).map_err(err)?.is_none() {
        return Err(not_found("novel"));
    }
    e.db.add_character(&id, name, b.role.trim()).map_err(err)?;
    Ok(Json(json!({"ok": true})))
}

#[derive(Deserialize)]
struct CharacterUpdateBody {
    #[serde(default)]
    role: String,
    #[serde(default = "default_active")]
    status: String,
    #[serde(default)]
    facts: Vec<String>,
}

fn default_active() -> String {
    "active".to_string()
}

async fn update_character(
    State(e): State<Arc<Engine>>,
    Path((_id, cid)): Path<(String, String)>,
    Json(b): Json<CharacterUpdateBody>,
) -> Result<Json<serde_json::Value>, ApiErr> {
    let ok = e
        .db
        .update_character(&cid, &b.role, &b.status, &b.facts)
        .map_err(err)?;
    if !ok {
        return Err(not_found("character"));
    }
    Ok(Json(json!({"ok": true})))
}

async fn delete_character(
    State(e): State<Arc<Engine>>,
    Path((_id, cid)): Path<(String, String)>,
) -> Result<Json<serde_json::Value>, ApiErr> {
    let ok = e.db.delete_character(&cid).map_err(err)?;
    if !ok {
        return Err(not_found("character"));
    }
    Ok(Json(json!({"ok": true})))
}

// ---------- 伏笔台账 ----------

async fn list_foreshadowings(
    State(e): State<Arc<Engine>>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiErr> {
    let items = e.db.list_foreshadowings(&id).map_err(err)?;
    Ok(Json(json!({"items": items})))
}

#[derive(Deserialize)]
struct ForeshadowingBody {
    title: String,
    #[serde(default)]
    notes: String,
}

async fn add_foreshadowing(
    State(e): State<Arc<Engine>>,
    Path(id): Path<String>,
    Json(b): Json<ForeshadowingBody>,
) -> Result<Json<serde_json::Value>, ApiErr> {
    let title = b.title.trim();
    if title.is_empty() {
        return Err((StatusCode::BAD_REQUEST, "伏笔内容不能为空".to_string()));
    }
    if e.db.get_novel(&id).map_err(err)?.is_none() {
        return Err(not_found("novel"));
    }
    e.db.add_foreshadowing(&id, title, b.notes.trim()).map_err(err)?;
    Ok(Json(json!({"ok": true})))
}

#[derive(Deserialize)]
struct ForeshadowingUpdateBody {
    #[serde(default = "default_open")]
    status: String,
    #[serde(default)]
    notes: String,
    #[serde(default)]
    resolved_chapter: Option<i64>,
}

fn default_open() -> String {
    "open".to_string()
}

async fn update_foreshadowing(
    State(e): State<Arc<Engine>>,
    Path((_id, fid)): Path<(String, String)>,
    Json(b): Json<ForeshadowingUpdateBody>,
) -> Result<Json<serde_json::Value>, ApiErr> {
    let status = if b.status == "resolved" { "resolved" } else { "open" };
    let resolved_chapter = if status == "resolved" { b.resolved_chapter } else { None };
    let ok = e
        .db
        .update_foreshadowing(&fid, status, &b.notes, resolved_chapter)
        .map_err(err)?;
    if !ok {
        return Err(not_found("foreshadowing"));
    }
    Ok(Json(json!({"ok": true})))
}

async fn delete_foreshadowing(
    State(e): State<Arc<Engine>>,
    Path((_id, fid)): Path<(String, String)>,
) -> Result<Json<serde_json::Value>, ApiErr> {
    let ok = e.db.delete_foreshadowing(&fid).map_err(err)?;
    if !ok {
        return Err(not_found("foreshadowing"));
    }
    Ok(Json(json!({"ok": true})))
}

// ---------- 运行记录 ----------

#[derive(Deserialize)]
struct RunsQuery {
    #[serde(default = "default_run_limit")]
    limit: i64,
}

fn default_run_limit() -> i64 {
    50
}

async fn list_runs(
    State(e): State<Arc<Engine>>,
    Query(q): Query<RunsQuery>,
) -> Result<Json<serde_json::Value>, ApiErr> {
    let items = e.db.list_runs(q.limit.clamp(1, 200)).map_err(err)?;
    Ok(Json(json!({"items": items})))
}

// ---------- 导出 ----------

async fn export_novel(
    State(e): State<Arc<Engine>>,
    Path(id): Path<String>,
) -> Result<impl IntoResponse, ApiErr> {
    let novel = e
        .db
        .get_novel(&id)
        .map_err(err)?
        .ok_or_else(|| not_found("novel"))?;
    let chapters = e.db.list_chapters(&id).map_err(err)?;
    let body = crate::export::novel_to_txt(&novel, &chapters);
    let short: String = id.chars().take(8).collect();
    let filename = format!("novel-{short}.txt");
    Ok((
        [
            (header::CONTENT_TYPE, "text/plain; charset=utf-8".to_string()),
            (
                header::CONTENT_DISPOSITION,
                format!("attachment; filename=\"{filename}\""),
            ),
        ],
        body,
    ))
}

// ---------- 用量统计 ----------

/// 书籍用量：总计 + 按任务 + 按章节（None = 该口径未知，前端显示「—」）
async fn usage_summary(
    State(e): State<Arc<Engine>>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiErr> {
    if e.db.get_novel(&id).map_err(err)?.is_none() {
        return Err(not_found("novel"));
    }
    let totals = e.db.usage_totals(Some(&id)).map_err(err)?;
    let by_task = e.db.usage_by_task(&id).map_err(err)?;
    let by_chapter = e.db.usage_by_chapter(&id).map_err(err)?;
    Ok(Json(json!({
        "totals": totals,
        "by_task": by_task,
        "by_chapter": by_chapter,
    })))
}

// ---------- 备份 / 恢复 ----------

/// 导出全量备份（zip：meta.json + ainovel.db 一致性快照）
async fn backup_db(State(e): State<Arc<Engine>>) -> Result<impl IntoResponse, ApiErr> {
    let dir = std::env::temp_dir().join(format!("ainovel-backup-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).map_err(err)?;
    let zip_path = dir.join("backup.zip");
    let size = match crate::backup::build_backup_zip(&e.db, &zip_path) {
        Ok(s) => s,
        Err(e2) => {
            let _ = std::fs::remove_dir_all(&dir);
            return Err(err(e2));
        }
    };
    let bytes = std::fs::read(&zip_path).map_err(err)?;
    let _ = std::fs::remove_dir_all(&dir);
    let filename = format!(
        "ainovel-backup-{}.zip",
        chrono::Utc::now().format("%Y%m%d-%H%M%S")
    );
    Ok((
        [
            (header::CONTENT_TYPE, "application/zip".to_string()),
            (
                header::CONTENT_DISPOSITION,
                format!("attachment; filename=\"{filename}\""),
            ),
            (header::CONTENT_LENGTH, size.to_string()),
        ],
        bytes,
    ))
}

/// 恢复备份：覆盖当前全部数据（换机迁移 / 误删回滚）
async fn restore_db(
    State(e): State<Arc<Engine>>,
    body: axum::body::Bytes,
) -> Result<Json<serde_json::Value>, ApiErr> {
    if body.is_empty() {
        return Err((StatusCode::BAD_REQUEST, "备份内容为空".to_string()));
    }
    let dir = std::env::temp_dir().join(format!("ainovel-restore-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).map_err(err)?;
    let zip_path = dir.join("restore.zip");
    let outcome = (|| -> anyhow::Result<usize> {
        std::fs::write(&zip_path, &body)?;
        let db_path = crate::backup::extract_backup_zip(&zip_path, &dir)?;
        e.db.restore_from_file(&db_path)?;
        Ok(e.db.list_novels()?.len())
    })();
    let _ = std::fs::remove_dir_all(&dir);
    let novels = outcome.map_err(|e2| (StatusCode::BAD_REQUEST, format!("{e2}")))?;
    Ok(Json(json!({"ok": true, "novels": novels})))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constant_time_eq_works() {
        assert!(constant_time_eq("abc", "abc"));
        assert!(!constant_time_eq("abc", "abd"));
        assert!(!constant_time_eq("abc", "abcd"));
        assert!(!constant_time_eq("", "a"));
        assert!(constant_time_eq("", ""));
        assert!(constant_time_eq("a1b2c3", "a1b2c3"));
    }
}
