use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use tower::ServiceExt;

use ainovel_engine::config::AppConfig;
use ainovel_engine::models::{ChapterPlan, LlmSettings};
use ainovel_engine::{api, Engine};

async fn json_body(resp: axum::response::Response) -> serde_json::Value {
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&bytes).unwrap()
}

/// 端到端（离线 Mock 模型）：建书 → 2 章流水线 → 审校 → 导出
#[tokio::test]
async fn mock_pipeline_end_to_end() {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("ainovel-test-{stamp}"));
    let engine = Engine::new(AppConfig::for_data_dir(dir.clone())).unwrap();
    engine.db.set_llm_settings(&LlmSettings::default()).unwrap();
    let app = api::build_router(engine.clone());

    // 1) 建书 + 开写（2 章）
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/novels")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::json!({"idea": "一个关于深海与鲸歌的故事", "chapters": 2}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let v = json_body(resp).await;
    let novel_id = v["novel_id"].as_str().unwrap().to_string();

    // 2) 轮询至完成
    let mut status = String::new();
    for _ in 0..400 {
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/api/novels/{novel_id}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let v = json_body(resp).await;
        status = v["run"]["status"].as_str().unwrap_or("").to_string();
        if status == "done" {
            break;
        }
        assert_ne!(status, "failed", "run failed: {}", v["run"]["error"]);
    }
    assert_eq!(status, "done", "pipeline did not finish in time");

    // 3) 章节内容与审校结果
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/api/novels/{novel_id}/chapters/1"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let v = json_body(resp).await;
    let content = v["content"].as_str().unwrap_or("");
    assert!(content.contains("第1章"), "content: {content}");
    assert!(v["review"].is_string(), "review should be stored");

    // 4) 导出
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/api/novels/{novel_id}/export"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let text = String::from_utf8_lossy(&bytes);
    assert!(text.contains("第1章"), "export: {text}");
    assert!(text.contains("第2章"), "export: {text}");

    let _ = std::fs::remove_dir_all(&dir);
}

/// 回归测试：继续创作时不得覆盖已写章节的规划（只更新仍处于 planned 的章节）。
#[tokio::test]
async fn upsert_does_not_clobber_written_chapters() {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("ainovel-test-upsert-{stamp}"));
    let engine = Engine::new(AppConfig::for_data_dir(dir.clone())).unwrap();
    let novel = engine.db.create_novel("测试").unwrap();

    let plan = |idx: i64, title: &str| ChapterPlan {
        idx,
        title: title.to_string(),
        goal: String::new(),
        beats: Vec::new(),
    };

    // 第 1 章：已写完；第 2 章：仅规划
    engine.db.upsert_chapter_plan(&novel.id, &plan(1, "旧标题")).unwrap();
    engine.db.save_chapter_content(&novel.id, 1, "正文", "done").unwrap();
    engine.db.upsert_chapter_plan(&novel.id, &plan(2, "计划标题")).unwrap();

    // 模拟"续规划"再次 upsert 同一批章节
    engine.db.upsert_chapter_plan(&novel.id, &plan(1, "新标题")).unwrap();
    engine.db.upsert_chapter_plan(&novel.id, &plan(2, "计划标题2")).unwrap();

    let c1 = engine.db.get_chapter(&novel.id, 1).unwrap().unwrap();
    let c2 = engine.db.get_chapter(&novel.id, 2).unwrap().unwrap();
    assert_eq!(c1.title, "旧标题", "已写章节的规划不应被覆盖");
    assert_eq!(c1.content, "正文", "已写章节的正文不应被覆盖");
    assert_eq!(c2.title, "计划标题2", "未写章节的规划应可更新");

    let _ = std::fs::remove_dir_all(&dir);
}
