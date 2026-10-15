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
    let novel = engine.db.create_novel("测试", "auto").unwrap();

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

/// 按阶段确认：规划完成后暂停，人工确认（可改章纲）后继续写正文。
#[tokio::test]
async fn staged_mode_pauses_after_outline() {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("ainovel-test-staged-{stamp}"));
    let engine = Engine::new(AppConfig::for_data_dir(dir.clone())).unwrap();
    engine.db.set_llm_settings(&LlmSettings::default()).unwrap();
    let novel = engine.db.create_novel("阶段确认测试", "staged").unwrap();

    ainovel_engine::pipeline::spawn(engine.clone(), &novel.id, 2).unwrap();

    let mut status = String::new();
    for _ in 0..200 {
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        status = engine
            .db
            .latest_run(&novel.id)
            .unwrap()
            .map(|r| r.status)
            .unwrap_or_default();
        if status == "paused" {
            break;
        }
        assert_ne!(status, "failed", "staged run failed");
    }
    assert_eq!(status, "paused", "规划后应暂停等待确认");

    let chapters = engine.db.list_chapters(&novel.id).unwrap();
    assert!(!chapters.is_empty(), "暂停时应已生成章节规划");
    assert!(
        chapters.iter().all(|c| c.content.trim().is_empty()),
        "暂停时不应开始写正文"
    );
    assert_eq!(
        engine.db.get_novel(&novel.id).unwrap().unwrap().status,
        "await_confirm"
    );

    // 人工修改第一章规划，然后继续
    engine
        .db
        .update_chapter_plan(&novel.id, 1, "人工改过的标题", "人工目标", &["节拍A".to_string()])
        .unwrap();
    ainovel_engine::pipeline::spawn(engine.clone(), &novel.id, 2).unwrap();
    for _ in 0..400 {
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        status = engine
            .db
            .latest_run(&novel.id)
            .unwrap()
            .map(|r| r.status)
            .unwrap_or_default();
        if status == "done" {
            break;
        }
        assert_ne!(status, "failed", "resume run failed");
    }
    assert_eq!(status, "done");
    let c1 = engine.db.get_chapter(&novel.id, 1).unwrap().unwrap();
    assert!(!c1.content.trim().is_empty(), "确认后应写入正文");
    assert_eq!(c1.title, "人工改过的标题", "人工编辑的规划应被保留");
    assert_eq!(c1.goal, "人工目标");

    let _ = std::fs::remove_dir_all(&dir);
}

/// API：选段修文 / 手动编辑 / 规划守卫 / 角色与伏笔台账 / 模式切换
#[tokio::test]
async fn polish_edit_and_ledger_endpoints() {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("ainovel-test-api2-{stamp}"));
    let engine = Engine::new(AppConfig::for_data_dir(dir.clone())).unwrap();
    engine.db.set_llm_settings(&LlmSettings::default()).unwrap();
    let app = api::build_router(engine.clone());

    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/novels")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::json!({"idea": "API 扩展测试", "chapters": 1}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let novel_id = json_body(resp).await["novel_id"].as_str().unwrap().to_string();

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
        assert_ne!(status, "failed");
    }
    assert_eq!(status, "done");

    // 台账：角色 + 伏笔（写后自动抽取）
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/api/novels/{novel_id}/characters"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let chars = json_body(resp).await;
    assert!(
        !chars["items"].as_array().unwrap().is_empty(),
        "角色台账应有数据: {chars}"
    );
    assert_eq!(chars["items"][0]["name"], "林深");

    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/api/novels/{novel_id}/foreshadowings"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let fores = json_body(resp).await;
    assert!(
        !fores["items"].as_array().unwrap().is_empty(),
        "伏笔台账应有数据: {fores}"
    );

    // 选段修文
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/novels/{novel_id}/chapters/1/polish"))
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::json!({"mode": "rewrite", "selection": "他走了过去，看了看窗户。"})
                        .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let v = json_body(resp).await;
    assert!(v["revised"].as_str().unwrap().contains("改写"));

    // 手动编辑正文
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri(format!("/api/novels/{novel_id}/chapters/1"))
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::json!({"content": "手动改写后的正文。"}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
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
    assert_eq!(json_body(resp).await["content"], "手动改写后的正文。");

    // 已开写章节改规划 → 409
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri(format!("/api/novels/{novel_id}/chapters/1/plan"))
                .header("content-type", "application/json")
                .body(Body::from(serde_json::json!({"title": "新标题"}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CONFLICT);

    // 切换推进方式
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PATCH")
                .uri(format!("/api/novels/{novel_id}"))
                .header("content-type", "application/json")
                .body(Body::from(serde_json::json!({"mode": "staged"}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let _ = std::fs::remove_dir_all(&dir);
}

/// 章节结构编辑（插入 / 移动 / 删除）+ 单章重新审校
#[tokio::test]
async fn chapter_structure_and_review_endpoints() {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("ainovel-test-struct-{stamp}"));
    let engine = Engine::new(AppConfig::for_data_dir(dir.clone())).unwrap();
    engine.db.set_llm_settings(&LlmSettings::default()).unwrap();
    let app = api::build_router(engine.clone());

    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/novels")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::json!({"idea": "结构编辑测试", "chapters": 2}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let novel_id = json_body(resp).await["novel_id"].as_str().unwrap().to_string();

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
        assert_ne!(status, "failed");
    }
    assert_eq!(status, "done");

    // 在第 1 章后插入新章
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/novels/{novel_id}/chapters"))
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::json!({"after_idx": 1, "title": "插入的新章", "goal": "插入目标"}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(json_body(resp).await["idx"], 2);

    let titles = |v: &serde_json::Value| -> Vec<String> {
        v["chapters"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| c["title"].as_str().unwrap_or("").to_string())
            .collect()
    };
    let fetch = |app: axum::Router, novel_id: String| async move {
        let resp = app
            .oneshot(
                Request::builder()
                    .uri(format!("/api/novels/{novel_id}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        json_body(resp).await
    };

    let v = fetch(app.clone(), novel_id.clone()).await;
    assert_eq!(titles(&v), vec!["风起之1", "插入的新章", "风起之2"]);

    // 下移插入的新章
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri(format!("/api/novels/{novel_id}/chapters/2/move"))
                .header("content-type", "application/json")
                .body(Body::from(serde_json::json!({"direction": "down"}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let v = fetch(app.clone(), novel_id.clone()).await;
    assert_eq!(titles(&v), vec!["风起之1", "风起之2", "插入的新章"]);

    // 单章重新审校
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/novels/{novel_id}/chapters/1/review"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let v = json_body(resp).await;
    assert_eq!(v["review"]["ok"], true);

    // 删除第 2 章
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri(format!("/api/novels/{novel_id}/chapters/2"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let v = fetch(app.clone(), novel_id.clone()).await;
    assert_eq!(titles(&v), vec!["风起之1", "插入的新章"]);
    let idxs: Vec<i64> = v["chapters"].as_array().unwrap().iter().map(|c| c["idx"].as_i64().unwrap()).collect();
    assert_eq!(idxs, vec![1, 2]);

    // 删到只剩一章后再删 → 409
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri(format!("/api/novels/{novel_id}/chapters/2"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri(format!("/api/novels/{novel_id}/chapters/1"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CONFLICT);

    let _ = std::fs::remove_dir_all(&dir);
}

/// 写作工具：光标续写 / 章节重写 / 单章资料整理 / 书籍体检
#[tokio::test]
async fn writing_tools_continue_rewrite_extract_health() {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("ainovel-test-tools-{stamp}"));
    let engine = Engine::new(AppConfig::for_data_dir(dir.clone())).unwrap();
    engine.db.set_llm_settings(&LlmSettings::default()).unwrap();
    let app = api::build_router(engine.clone());

    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/novels")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::json!({"idea": "写作工具测试", "chapters": 2}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let novel_id = json_body(resp).await["novel_id"].as_str().unwrap().to_string();

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
        assert_ne!(status, "failed");
    }
    assert_eq!(status, "done");

    // 光标续写
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/novels/{novel_id}/chapters/2/continue"))
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::json!({"before": "雨还在下，他站在巷口。", "chars": 300}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let v = json_body(resp).await;
    assert!(v["text"].as_str().unwrap().contains("续写"), "continue: {v}");

    // 章节重写：重新生成
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/novels/{novel_id}/chapters/1/rewrite"))
                .header("content-type", "application/json")
                .body(Body::from(serde_json::json!({"mode": "regenerate"}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let v = json_body(resp).await;
    assert!(v["text"].as_str().unwrap().contains("重新生成"), "rewrite: {v}");

    // 章节重写：按意见改写
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/novels/{novel_id}/chapters/1/rewrite"))
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::json!({"mode": "revise", "instruction": "更口语一点"}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert!(json_body(resp).await["text"].as_str().unwrap().contains("修订版"));

    // 单章资料整理 → 摘要入库
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/novels/{novel_id}/chapters/1/extract"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let v = json_body(resp).await;
    assert!(v["characters"].as_u64().unwrap() >= 1);
    assert!(!v["summary"].as_str().unwrap().is_empty());

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
    assert!(!json_body(resp).await["summary"].as_str().unwrap().is_empty());

    // 书籍体检
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/api/novels/{novel_id}/health"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let v = json_body(resp).await;
    assert_eq!(v["chapters_total"], 2);
    assert_eq!(v["chapters_done"], 2);
    assert!(v["open_foreshadowings"].as_u64().unwrap() >= 1);
    assert!(!v["suggestions"].as_array().unwrap().is_empty());

    let _ = std::fs::remove_dir_all(&dir);
}
