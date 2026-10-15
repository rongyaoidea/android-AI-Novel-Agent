use std::convert::Infallible;
use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::{header, StatusCode};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::json;

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

pub fn build_router(engine: Arc<Engine>) -> Router {
    Router::new()
        .route("/", get(index))
        .route("/app.css", get(app_css))
        .route("/app.js", get(app_js))
        .route("/api/health", get(health))
        .route("/api/settings/llm", get(get_llm).put(put_llm))
        .route("/api/settings/llm/test", post(test_llm))
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
        .route("/api/runs", get(list_runs))
        .with_state(engine)
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

async fn get_llm(State(e): State<Arc<Engine>>) -> Result<Json<serde_json::Value>, ApiErr> {
    let s = e.db.llm_settings().map_err(err)?;
    Ok(Json(serde_json::to_value(s).map_err(err)?))
}

async fn put_llm(
    State(e): State<Arc<Engine>>,
    Json(s): Json<LlmSettings>,
) -> Result<Json<serde_json::Value>, ApiErr> {
    e.db.set_llm_settings(&s).map_err(err)?;
    Ok(Json(json!({"ok": true})))
}

async fn test_llm(State(e): State<Arc<Engine>>) -> Result<Json<serde_json::Value>, ApiErr> {
    let settings = e.db.llm_settings().map_err(err)?;
    let llm = crate::llm::Llm::new(settings);
    match llm.complete("你是一个连通性测试助手。", "只回复两个字：正常").await {
        Ok(text) => Ok(Json(json!({
            "ok": true,
            "message": text.chars().take(80).collect::<String>(),
        }))),
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
    let novel = e.db.create_novel(idea, mode).map_err(err)?;
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
    let settings = e.db.llm_settings().map_err(err)?;
    let llm = crate::llm::Llm::new(settings);
    let (sys, usr) = crate::prompts::polish_prompt(&mode, sel, &ch.title, &b.instruction);
    let revised = llm.complete(&sys, &usr).await.map_err(err)?;
    Ok(Json(json!({"revised": revised.trim(), "mode": mode})))
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
    let settings = e.db.llm_settings().map_err(err)?;
    let llm = crate::llm::Llm::new(settings);
    let (sys, usr) = crate::prompts::review_prompt(&ch.content);
    let raw = llm.complete(&sys, &usr).await.map_err(err)?;
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
    e.hub.publish(pipeline::EngineEvent::ChapterReview {
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
    let settings = e.db.llm_settings().map_err(err)?;
    let llm = crate::llm::Llm::new(settings);
    let (sys, usr) = crate::prompts::continue_prompt(&ch.title, &before, chars, &b.instruction);
    let out = llm.complete(&sys, &usr).await.map_err(err)?;
    Ok(Json(json!({"text": out.trim(), "chars": chars})))
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
    let settings = e.db.llm_settings().map_err(err)?;
    let llm = crate::llm::Llm::new(settings);
    let (sys, usr) = crate::prompts::rewrite_prompt(mode, &ch.title, &ch.goal, &ch.content, &b.instruction);
    let out = llm.complete(&sys, &usr).await.map_err(err)?;
    Ok(Json(json!({"text": out.trim(), "mode": mode})))
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
    let settings = e.db.llm_settings().map_err(err)?;
    let llm = crate::llm::Llm::new(settings);
    let (sys, usr) = crate::prompts::extract_prompt(idx, &ch.content);
    let raw = llm.complete(&sys, &usr).await.map_err(err)?;
    let j = crate::llm::extract_json_object(&raw).ok_or_else(|| {
        (StatusCode::INTERNAL_SERVER_ERROR, "整理输出不是 JSON".to_string())
    })?;
    let (chars, fores) = pipeline::apply_extraction(&e.db, &id, idx, &j);
    e.hub.publish(pipeline::EngineEvent::LedgerUpdated {
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
    let stream = async_stream::stream! {
        if let Some(run) = snapshot {
            let payload = json!({"type": "snapshot", "run": run}).to_string();
            yield Ok(Event::default().event("engine").data(payload));
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
