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

pub fn build_router(engine: Arc<Engine>) -> Router {
    Router::new()
        .route("/", get(index))
        .route("/app.css", get(app_css))
        .route("/app.js", get(app_js))
        .route("/api/health", get(health))
        .route("/api/settings/llm", get(get_llm).put(put_llm))
        .route("/api/settings/llm/test", post(test_llm))
        .route("/api/novels", get(list_novels).post(create_novel))
        .route("/api/novels/:id", get(get_novel).delete(delete_novel))
        .route("/api/novels/:id/chapters/:idx", get(get_chapter))
        .route("/api/novels/:id/run", post(run_novel))
        .route("/api/novels/:id/events", get(events))
        .route("/api/novels/:id/export", get(export_novel))
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
    let novel = e.db.create_novel(idea).map_err(err)?;
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
        .ok_or_else(|| (StatusCode::NOT_FOUND, "novel not found".to_string()))?;
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

async fn delete_novel(
    State(e): State<Arc<Engine>>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiErr> {
    let ok = e.db.delete_novel(&id).map_err(err)?;
    if !ok {
        return Err((StatusCode::NOT_FOUND, "novel not found".to_string()));
    }
    Ok(Json(json!({"ok": true})))
}

async fn get_chapter(
    State(e): State<Arc<Engine>>,
    Path((id, idx)): Path<(String, i64)>,
) -> Result<Json<serde_json::Value>, ApiErr> {
    let ch = e
        .db
        .get_chapter(&id, idx)
        .map_err(err)?
        .ok_or_else(|| (StatusCode::NOT_FOUND, "chapter not found".to_string()))?;
    Ok(Json(json!({
        "idx": ch.idx,
        "title": ch.title,
        "goal": ch.goal,
        "beats": ch.beats,
        "content": ch.content,
        "words": ch.content.chars().count(),
        "status": ch.status,
        "review": ch.review_json,
    })))
}

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
        return Err((StatusCode::NOT_FOUND, "novel not found".to_string()));
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
        .ok_or_else(|| (StatusCode::NOT_FOUND, "novel not found".to_string()))?;
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
