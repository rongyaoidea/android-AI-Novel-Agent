use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use tower::ServiceExt;

use ainovel_engine::config::AppConfig;
use ainovel_engine::models::{ChapterPlan, GlobalIssue, GlobalReview, LlmSettings, PipelineSettings};
use ainovel_engine::pipeline::EngineEvent;
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
    let novel = engine.db.create_novel("测试", "auto", "").unwrap();

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
    let novel = engine.db.create_novel("阶段确认测试", "staged", "").unwrap();

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

/// 数据安全 ①：token 用量统计口径（缓存命中 / 未命中 / 合计必须自洽）
#[tokio::test]
async fn usage_accounting_is_correct() {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("ainovel-test-usage-{stamp}"));
    let engine = Engine::new(AppConfig::for_data_dir(dir.clone())).unwrap();
    engine.db.set_llm_settings(&LlmSettings::default()).unwrap();
    let app = api::build_router(engine.clone());

    // 建书跑完整流水线（mock 上报确定性用量）
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/novels")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::json!({"idea": "用量统计测试", "chapters": 2}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let novel_id = json_body(resp).await["novel_id"].as_str().unwrap().to_string();
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
        let status = v["run"]["status"].as_str().unwrap_or("");
        if status == "done" {
            break;
        }
        assert_ne!(status, "failed");
    }

    // 再补一次选段修文，验证不同任务都入账
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

    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/api/novels/{novel_id}/usage"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let v = json_body(resp).await;
    let t = &v["totals"];

    let calls = t["calls"].as_i64().unwrap();
    // 立项 + 规划 + 2 章正文 + 2 次审校 + 2 次抽取 + 1 次修文 = 9
    assert!(calls >= 9, "calls = {calls}, payload: {v}");

    let input = t["input"].as_i64().expect("mock 必须上报 input");
    let output = t["output"].as_i64().expect("mock 必须上报 output");
    let hit = t["cache_hit"].as_i64().expect("mock 必须上报 cache_hit");
    let miss = t["cache_miss"].as_i64().expect("mock 必须上报 cache_miss");
    let total = t["total"].as_i64().expect("mock 必须上报 total");

    assert_eq!(hit + miss, input, "命中 + 未命中必须等于输入总量");
    assert_eq!(input + output, total, "输入 + 输出必须等于合计");
    assert!(hit > 0, "mock 应产生缓存命中");
    assert!(miss > 0, "mock 应产生未命中部分");

    // 分组：任务维度含正文生成与修文；章节维度含第 1、2 章
    let tasks: Vec<String> = v["by_task"]
        .as_array()
        .unwrap()
        .iter()
        .map(|g| g["key"].as_str().unwrap().to_string())
        .collect();
    for want in ["brief", "outline", "chapter", "review", "extract", "polish"] {
        assert!(tasks.contains(&want.to_string()), "缺少任务分组 {want}: {tasks:?}");
    }
    let chapter_keys: Vec<String> = v["by_chapter"]
        .as_array()
        .unwrap()
        .iter()
        .map(|g| g["key"].as_str().unwrap().to_string())
        .collect();
    assert!(chapter_keys.contains(&"第1章".to_string()), "{chapter_keys:?}");
    assert!(chapter_keys.contains(&"第2章".to_string()), "{chapter_keys:?}");

    // 每行分组同样自洽
    for g in v["by_task"].as_array().unwrap() {
        let gi = g["input"].as_i64().unwrap();
        let gh = g["cache_hit"].as_i64().unwrap();
        let gm = g["cache_miss"].as_i64().unwrap();
        assert_eq!(gh + gm, gi, "分组口径不自洽: {g}");
    }

    let _ = std::fs::remove_dir_all(&dir);
}

/// 数据安全 ②：本地服务令牌（无令牌 401；头 / Bearer / 查询参数三种方式放行；health 放行）
#[tokio::test]
async fn auth_token_protects_api() {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("ainovel-test-auth-{stamp}"));
    let config = AppConfig::for_data_dir(dir.clone())
        .with_security(Some("test-token-123456".into()), None);
    let engine = Engine::new(config).unwrap();
    engine.db.set_llm_settings(&LlmSettings::default()).unwrap();
    let app = api::build_router(engine.clone());

    let call = |app: axum::Router, uri: &str, token: Option<&str>| {
        let uri = uri.to_string();
        let token = token.map(|t| t.to_string());
        async move {
            let mut req = Request::builder().uri(uri);
            if let Some(t) = token {
                req = req.header("x-ainovel-token", t);
            }
            app.oneshot(req.body(Body::empty()).unwrap()).await.unwrap()
        }
    };

    // 无令牌 → 401
    let resp = call(app.clone(), "/api/novels", None).await;
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    // 错误令牌 → 401
    let resp = call(app.clone(), "/api/novels", Some("wrong")).await;
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    // 正确令牌（头）→ 200
    let resp = call(app.clone(), "/api/novels", Some("test-token-123456")).await;
    assert_eq!(resp.status(), StatusCode::OK);
    // 正确令牌（查询参数，EventSource 场景）→ 200
    let resp = call(app.clone(), "/api/novels?token=test-token-123456", None).await;
    assert_eq!(resp.status(), StatusCode::OK);
    // Authorization: Bearer → 200
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/novels")
                .header("authorization", "Bearer test-token-123456")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    // health 与静态资源无需令牌
    let resp = call(app.clone(), "/api/health", None).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let resp = call(app.clone(), "/", None).await;
    assert_eq!(resp.status(), StatusCode::OK);

    let _ = std::fs::remove_dir_all(&dir);
}

/// 数据安全 ③：备份导出 / 恢复覆盖（HTTP 全链路）
#[tokio::test]
async fn backup_and_restore_endpoints() {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("ainovel-test-backup-{stamp}"));
    let engine = Engine::new(AppConfig::for_data_dir(dir.clone())).unwrap();
    let app = api::build_router(engine.clone());

    // 造一本带正文的书 → 导出备份
    let novel = engine.db.create_novel("备份的书", "auto", "").unwrap();
    engine
        .db
        .upsert_chapter_plan(
            &novel.id,
            &ChapterPlan {
                idx: 1,
                title: "首章".into(),
                goal: String::new(),
                beats: vec![],
            },
        )
        .unwrap();
    engine.db.save_chapter_content(&novel.id, 1, "备份前的正文", "done").unwrap();

    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/backup")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let backup_bytes = resp.into_body().collect().await.unwrap().to_bytes();
    assert!(backup_bytes.len() > 100, "备份包太小: {}", backup_bytes.len());
    assert_eq!(&backup_bytes[..2], b"PK", "备份应为 zip 格式");

    // 再建一本新书（恢复后应被覆盖掉）
    engine.db.create_novel("将被覆盖的书", "auto", "").unwrap();
    assert_eq!(engine.db.list_novels().unwrap().len(), 2);

    // 非 zip → 400
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/restore")
                .header("content-type", "application/zip")
                .body(Body::from("not a zip"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

    // 恢复备份 → 只剩备份里的书
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/restore")
                .header("content-type", "application/zip")
                .body(Body::from(backup_bytes.clone()))
                .unwrap(),
        )
        .await
        .unwrap();
    let restore_status = resp.status();
    let restore_body = json_body(resp).await;
    assert_eq!(restore_status, StatusCode::OK, "restore body: {restore_body}");
    let items = engine.db.list_novels().unwrap();
    assert_eq!(items.len(), 1, "恢复后应只剩备份中的作品");
    assert_eq!(items[0].novel.idea, "备份的书");
    let ch = engine.db.get_chapter(&items[0].novel.id, 1).unwrap().unwrap();
    assert_eq!(ch.content, "备份前的正文", "正文应随备份恢复");

    let _ = std::fs::remove_dir_all(&dir);
}

/// 数据安全 ④：API Key 落库加密（明文不出现在数据库中；换密钥后无法解密）
#[tokio::test]
async fn api_key_is_encrypted_at_rest() {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("ainovel-test-crypt-{stamp}"));
    let secret_hex = "ab".repeat(32);
    let config = AppConfig::for_data_dir(dir.clone())
        .with_security(None, Some(secret_hex.clone()));
    let engine = Engine::new(config).unwrap();
    let app = api::build_router(engine.clone());

    // 通过 API 保存 Key
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/api/settings/llm")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "provider": "openai",
                        "base_url": "https://api.deepseek.com/v1",
                        "api_key": "sk-plaintext-secret",
                        "model": "deepseek-chat",
                        "temperature": 0.8
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    // 原始落库值：必须是密文前缀，且不含明文
    let raw = engine.db.llm_settings().unwrap();
    assert!(
        raw.api_key.starts_with("enc:v1:"),
        "落库应为密文: {}",
        raw.api_key
    );
    assert!(!raw.api_key.contains("sk-plaintext-secret"));

    // 读取接口：自动解密
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/settings/llm")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(json_body(resp).await["api_key"], "sk-plaintext-secret");

    // 引擎层读取同样解密
    assert_eq!(engine.llm_settings().unwrap().api_key, "sk-plaintext-secret");

    // 换一把密钥（模拟 Keystore 丢失/换机）：解密失败 → 置空并告警，不会泄漏乱码
    let other = Engine::new(
        AppConfig::for_data_dir(dir.clone())
            .with_security(None, Some("cd".repeat(32))),
    )
    .unwrap();
    assert_eq!(other.llm_settings().unwrap().api_key, "");

    let _ = std::fs::remove_dir_all(&dir);
}

/// 文风预设：目录接口 + 建书绑定 + 工作台更换（含非法值拒绝与清除）
#[tokio::test]
async fn style_presets_endpoints() {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("ainovel-test-style-{stamp}"));
    let engine = Engine::new(AppConfig::for_data_dir(dir.clone())).unwrap();
    engine.db.set_llm_settings(&LlmSettings::default()).unwrap();
    let app = api::build_router(engine.clone());

    // 1) 目录：数量、字段齐全、id 唯一
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/styles")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let v = json_body(resp).await;
    let items = v["items"].as_array().unwrap();
    assert!(items.len() >= 8, "预设数量不足: {}", items.len());
    let mut ids: Vec<String> = items
        .iter()
        .map(|p| p["id"].as_str().unwrap().to_string())
        .collect();
    let n = ids.len();
    ids.sort();
    ids.dedup();
    assert_eq!(ids.len(), n, "预设 id 必须唯一");
    for p in items {
        for key in ["id", "name", "group", "tagline", "genres", "rules", "avoid"] {
            assert!(!p[key].is_null(), "预设缺字段 {key}: {p}");
        }
        assert!(!p["rules"].as_array().unwrap().is_empty());
    }
    // 每组频道至少有一套
    for group in ["男频", "女频", "通用"] {
        assert!(
            items.iter().any(|p| p["group"] == group),
            "缺少 {group} 频道预设"
        );
    }

    // 2) 建书时绑定预设 → 落库并在详情回显
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/novels")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::json!({"idea": "文风预设测试", "chapters": 1, "style_preset": "gufeng"})
                        .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let novel_id = json_body(resp).await["novel_id"].as_str().unwrap().to_string();
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
    assert_eq!(v["novel"]["style_preset"], "gufeng");

    // 流水线照常完成（风格块不应污染 Mock 解析）
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
    assert_eq!(status, "done");
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
    assert!(v["content"].as_str().unwrap().contains("第1章"), "正文章节标题应正常");

    // 3) 更换预设 → 200；清除 → 空串；非法值 → 400 且不改变现状
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PATCH")
                .uri(format!("/api/novels/{novel_id}"))
                .header("content-type", "application/json")
                .body(Body::from(serde_json::json!({"style_preset": "rexue"}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(
        engine.db.get_novel(&novel_id).unwrap().unwrap().style_preset,
        "rexue"
    );

    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PATCH")
                .uri(format!("/api/novels/{novel_id}"))
                .header("content-type", "application/json")
                .body(Body::from(serde_json::json!({"style_preset": "no-such-style"}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        engine.db.get_novel(&novel_id).unwrap().unwrap().style_preset,
        "rexue",
        "非法值不应改变现状"
    );

    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PATCH")
                .uri(format!("/api/novels/{novel_id}"))
                .header("content-type", "application/json")
                .body(Body::from(serde_json::json!({"style_preset": ""}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(
        engine.db.get_novel(&novel_id).unwrap().unwrap().style_preset,
        ""
    );

    // 4) 建书时传非法预设 → 静默按「不限」处理（不阻断建书）
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/novels")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::json!({"idea": "非法预设", "chapters": 1, "style_preset": "xxx"}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let nid2 = json_body(resp).await["novel_id"].as_str().unwrap().to_string();
    assert_eq!(engine.db.get_novel(&nid2).unwrap().unwrap().style_preset, "");

    let _ = std::fs::remove_dir_all(&dir);
}

/// 提示词模板：目录 / 校验 / 覆盖立即生效（含 Mock 标记兜底）/ 恢复默认
#[tokio::test]
async fn prompt_overrides_endpoints() {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("ainovel-test-prompts-{stamp}"));
    let engine = Engine::new(AppConfig::for_data_dir(dir.clone())).unwrap();
    engine.db.set_llm_settings(&LlmSettings::default()).unwrap();
    let app = api::build_router(engine.clone());

    // 1) 目录：12 个任务，默认模板齐全，初始无覆盖
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/prompts")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let v = json_body(resp).await;
    let items = v["items"].as_array().unwrap();
    assert_eq!(items.len(), 14, "任务数量应为 14");
    for t in items {
        assert!(!t["default_system"].as_str().unwrap().is_empty(), "{}", t["id"]);
        assert!(!t["default_user"].as_str().unwrap().is_empty(), "{}", t["id"]);
        assert!(t["vars"].as_array().unwrap().len() >= 2, "{}", t["id"]);
        assert!(t["custom"].is_null(), "{} 初始不应有覆盖", t["id"]);
    }

    // 2) 非法输入：未知任务 404 / 未知占位符 400 / 空 system 400
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/api/prompts/bogus")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::json!({"system": "s", "user": "u"}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);

    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/api/prompts/chapter")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::json!({"system": "s", "user": "写 {{bogus}}"}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/api/prompts/chapter")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::json!({"system": "  ", "user": "写一章"}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

    // 3) 合法覆盖：故意不写 TASK 标记，验证 Mock 兜底仍能跑流水线
    let custom_user = "请写第 {{idx}} 章「{{title}}」，题材：{{genre}}，字数：{{words}}。";
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/api/prompts/chapter")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::json!({"system": "自定义系统提示", "user": custom_user}).to_string(),
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
                .uri("/api/prompts")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let v = json_body(resp).await;
    let chapter_task = v["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["id"] == "chapter")
        .unwrap();
    assert_eq!(chapter_task["custom"]["user"], custom_user, "覆盖应被保存");

    // 引擎侧覆盖可读
    assert!(engine.prompt_overrides().get("chapter").is_some());

    // 4) 覆盖生效：建书跑流水线，Mock 因自动补标记仍可解析
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/novels")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::json!({"idea": "提示词覆盖测试", "chapters": 1}).to_string(),
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
        assert_ne!(status, "failed", "run failed: {}", v["run"]["error"]);
    }
    assert_eq!(status, "done", "自定义模板下流水线应照常完成");
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
    assert!(v["content"].as_str().unwrap().contains("第1章"), "Mock 应仍能生成正文");

    // 5) 恢复默认
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri("/api/prompts/chapter")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert!(engine.prompt_overrides().get("chapter").is_none());
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/prompts")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let v = json_body(resp).await;
    let chapter_task = v["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["id"] == "chapter")
        .unwrap();
    assert!(chapter_task["custom"].is_null(), "恢复后应无覆盖");

    let _ = std::fs::remove_dir_all(&dir);
}

/// 本地文风检查 + 自动重写：报告落库、意见并入审校、重写计入用量且只重试一次
#[tokio::test]
async fn style_report_and_auto_retry() {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("ainovel-test-stylecheck-{stamp}"));
    let engine = Engine::new(AppConfig::for_data_dir(dir.clone())).unwrap();
    engine.db.set_llm_settings(&LlmSettings::default()).unwrap();
    let app = api::build_router(engine.clone());

    // 建书：绑定「发疯毒舌」（要求对话占比 ≥ 42%，Mock 正文无对话 → 必触发自动重写）
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/novels")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::json!({"idea": "文风检查测试", "chapters": 1, "style_preset": "shadiao"})
                        .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let novel_id = json_body(resp).await["novel_id"].as_str().unwrap().to_string();

    let mut status = String::new();
    for _ in 0..500 {
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
    assert_eq!(status, "done", "自动重写不应导致运行失败");
    // 章节详情：文风报告存在且判不通过（Mock 段落等长 + 无对话）
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
    let report = &v["style_report"];
    assert!(!report.is_null(), "应写入文风检查报告: {v}");
    assert_eq!(report["style"], "shadiao");
    assert_eq!(report["hard_fail"], true, "Mock 文本应命中硬指标: {report}");
    let failed: Vec<&str> = report["checks"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|c| c["passed"] == false)
        .map(|c| c["name"].as_str().unwrap())
        .collect();
    assert!(failed.iter().any(|n| *n == "段落长短交错"), "{failed:?}");
    assert!(failed.iter().any(|n| *n == "对话占比"), "{failed:?}");

    // 审校意见应并入「文风检查：…」
    let review_raw = v["review"].as_str().unwrap_or("");
    assert!(review_raw.contains("文风检查"), "审校应包含文风检查意见: {review_raw}");

    // 用量：正文 1 次 + 重写 1 次（且只重试一次）
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/api/novels/{novel_id}/usage"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let u = json_body(resp).await;
    let task_calls = |key: &str| -> i64 {
        u["by_task"]
            .as_array()
            .unwrap()
            .iter()
            .find(|g| g["key"] == key)
            .map(|g| g["calls"].as_i64().unwrap_or(0))
            .unwrap_or(0)
    };
    assert_eq!(task_calls("chapter"), 1, "正文应只生成一次");
    assert_eq!(task_calls("chapter_retry"), 1, "硬指标不通过应重写一次（且仅一次）: {u}");

    let _ = std::fs::remove_dir_all(&dir);
}

/// 拉取服务商模型列表（API 全链路：表单参数 → 假服务商 /v1/models → 排序结果）
#[tokio::test]
async fn llm_model_list_endpoint() {
    // 假服务商：只有 /v1/models 可达
    let provider = axum::Router::new().route(
        "/v1/models",
        axum::routing::get(|| async {
            axum::Json(serde_json::json!({
                "object": "list",
                "data": [
                    {"id": "zz-last", "object": "model"},
                    {"id": "aa-first", "object": "model"}
                ]
            }))
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, provider).await;
    });

    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("ainovel-test-models-{stamp}"));
    let engine = Engine::new(AppConfig::for_data_dir(dir.clone())).unwrap();
    let app = api::build_router(engine.clone());

    // 表单只填了域名（无 /v1）→ 应自动回退并成功
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/settings/llm/models")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::json!({"base_url": format!("http://{addr}"), "api_key": "sk-test"})
                        .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let v = json_body(resp).await;
    assert_eq!(v["ok"], true, "{v}");
    assert_eq!(v["models"][0], "aa-first");
    assert_eq!(v["models"][1], "zz-last");

    // 无 Base URL 且未保存过 → 400（表单与存档都为空）
    engine
        .db
        .set_llm_settings(&LlmSettings::default())
        .unwrap();
    engine
        .db
        .set_llm_settings(&LlmSettings {
            provider: "mock".into(),
            base_url: String::new(),
            api_key: String::new(),
            model: String::new(),
            temperature: 0.8,
        })
        .unwrap();
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/settings/llm/models")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::json!({}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

    let _ = std::fs::remove_dir_all(&dir);
}


/// 全阶段事件可见 + 审校自动修复 + 写作偏好开关
#[tokio::test]
async fn stage_events_and_auto_fix() {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("ainovel-test-autofix-{stamp}"));
    let engine = Engine::new(AppConfig::for_data_dir(dir.clone())).unwrap();
    engine.db.set_llm_settings(&LlmSettings::default()).unwrap();
    let app = api::build_router(engine.clone());

    // 写作偏好：默认开启自动修复
    let resp = app
        .clone()
        .oneshot(Request::builder().uri("/api/settings/pipeline").body(Body::empty()).unwrap())
        .await
        .unwrap();
    let v = json_body(resp).await;
    assert_eq!(v["auto_fix_review"], true, "默认应开启自动修复: {v}");

    // 跑一章（Mock 审校固定返回 1 条 medium → 触发一次自动修复）
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/novels")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::json!({"idea": "自动修复测试", "chapters": 1}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let n1 = json_body(resp).await["novel_id"].as_str().unwrap().to_string();
    let mut status = String::new();
    for _ in 0..600 {
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        let resp = app
            .clone()
            .oneshot(Request::builder().uri(format!("/api/novels/{n1}")).body(Body::empty()).unwrap())
            .await
            .unwrap();
        let v = json_body(resp).await;
        status = v["run"]["status"].as_str().unwrap_or("").to_string();
        if status == "done" {
            break;
        }
        assert_ne!(status, "failed", "run failed: {}", v["run"]["error"]);
    }
    assert_eq!(status, "done");

    // 关键阶段事件全部出现在历史（SSE 重连回放同源）
    let hist = engine.hub.history(&n1);
    let stages: Vec<String> = hist
        .iter()
        .filter_map(|e| match e {
            EngineEvent::Stage { stage, .. } => Some(stage.clone()),
            _ => None,
        })
        .collect();
    for want in ["brief", "outline", "draft", "review", "autofix", "extract"] {
        assert!(stages.contains(&want.to_string()), "阶段事件 {want} 缺失: {stages:?}");
    }

    // 自动修复计入用量（每章一次）
    let resp = app
        .clone()
        .oneshot(Request::builder().uri(format!("/api/novels/{n1}/usage")).body(Body::empty()).unwrap())
        .await
        .unwrap();
    let u = json_body(resp).await;
    let calls_of = |v: &serde_json::Value, key: &str| -> i64 {
        v["by_task"]
            .as_array()
            .unwrap()
            .iter()
            .find(|g| g["key"] == key)
            .map(|g| g["calls"].as_i64().unwrap_or(0))
            .unwrap_or(0)
    };
    assert_eq!(calls_of(&u, "auto_fix"), 1, "应有一次自动修复调用: {u}");

    // 关掉自动修复：新书不再产生自动修复调用，也不再发出 autofix 阶段事件
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/api/settings/pipeline")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::json!({"auto_fix_review": false}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/novels")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::json!({"idea": "关闭自动修复测试", "chapters": 1}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let n2 = json_body(resp).await["novel_id"].as_str().unwrap().to_string();
    let mut status2 = String::new();
    for _ in 0..600 {
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        let resp = app
            .clone()
            .oneshot(Request::builder().uri(format!("/api/novels/{n2}")).body(Body::empty()).unwrap())
            .await
            .unwrap();
        let v = json_body(resp).await;
        status2 = v["run"]["status"].as_str().unwrap_or("").to_string();
        if status2 == "done" {
            break;
        }
        assert_ne!(status2, "failed");
    }
    assert_eq!(status2, "done");

    let hist2 = engine.hub.history(&n2);
    let has_autofix = hist2.iter().any(|e| matches!(e, EngineEvent::Stage { stage, .. } if stage == "autofix"));
    assert!(!has_autofix, "关闭后不应出现 autofix 阶段事件");
    let resp = app
        .clone()
        .oneshot(Request::builder().uri(format!("/api/novels/{n2}/usage")).body(Body::empty()).unwrap())
        .await
        .unwrap();
    let u2 = json_body(resp).await;
    assert_eq!(calls_of(&u2, "auto_fix"), 0, "关闭后不应有自动修复调用: {u2}");

    let _ = std::fs::remove_dir_all(&dir);
}

/// 自动连载：分批续写至目标自动停止；月度 Token 预算耗尽自动暂停
#[tokio::test]
async fn auto_continue_chain_and_budget() {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("ainovel-test-auto-{stamp}"));
    let engine = Engine::new(AppConfig::for_data_dir(dir.clone())).unwrap();
    engine.db.set_llm_settings(&LlmSettings::default()).unwrap();
    let app = api::build_router(engine.clone());

    let wait_done = |app: axum::Router, id: String| async move {
        let mut status = String::new();
        for _ in 0..900 {
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
            let resp = app
                .clone()
                .oneshot(Request::builder().uri(format!("/api/novels/{id}")).body(Body::empty()).unwrap())
                .await
                .unwrap();
            let v = json_body(resp).await;
            status = v["run"]["status"].as_str().unwrap_or("").to_string();
            if status == "done" {
                return v;
            }
            assert_ne!(status, "failed", "run failed: {}", v["run"]["error"]);
        }
        panic!("run did not finish: {status}");
    };

    // ---- ① 自动连载：1 章起步 → 目标 3 章 ----
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/novels")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::json!({"idea": "自动连载测试", "chapters": 1}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let n1 = json_body(resp).await["novel_id"].as_str().unwrap().to_string();
    let v = wait_done(app.clone(), n1.clone()).await;
    assert_eq!(v["novel"]["target_chapters"], 1, "创建时应设定目标章节数");
    assert_eq!(v["novel"]["auto_continue"], false);

    // 每批 1 章（默认 10）：让续写链走两步，覆盖 auto 阶段事件
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/api/settings/pipeline")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::json!({"auto_fix_review": true, "monthly_budget_tokens": 0, "auto_batch": 1})
                        .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    // 调大目标并启动自动连载
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PATCH")
                .uri(format!("/api/novels/{n1}"))
                .header("content-type", "application/json")
                .body(Body::from(serde_json::json!({"target_chapters": 3}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let resp = app
        .clone()
        .oneshot(Request::builder().method("POST").uri(format!("/api/novels/{n1}/auto")).body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let v = json_body(resp).await;
    assert_eq!(v["target"], 3);
    assert_eq!(v["batch_until"], 2, "每批 1 章：已完成 1 章 → 本批写到第 2 章");

    // 等连载收尾（自动写第 2、3 章并自动关闭开关）
    let mut finished = false;
    for _ in 0..900 {
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        let resp = app
            .clone()
            .oneshot(Request::builder().uri(format!("/api/novels/{n1}")).body(Body::empty()).unwrap())
            .await
            .unwrap();
        let v = json_body(resp).await;
        let auto = v["novel"]["auto_continue"].as_bool().unwrap_or(false);
        let status = v["run"]["status"].as_str().unwrap_or("");
        let done = v["chapters"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|c| c["status"] == "done")
            .count();
        if !auto && status == "done" && done == 3 {
            finished = true;
            break;
        }
        assert_ne!(status, "failed", "chain run failed");
    }
    assert!(finished, "自动连载未在时限内达到目标");

    // 两个运行（首批 + 自动批）与自动连载阶段事件
    let runs = engine.db.list_runs(50).unwrap();
    let count = runs.iter().filter(|it| it.run.novel_id == n1).count();
    assert!(count >= 3, "每批 1 章 → 首批 + 2 个自动批，实际 {count}");
    let stages: Vec<String> = engine.hub.history(&n1)
        .iter()
        .filter_map(|e| match e {
            EngineEvent::Stage { stage, .. } => Some(stage.clone()),
            _ => None,
        })
        .collect();
    assert!(stages.contains(&"auto".to_string()), "{stages:?}");
    assert!(stages.contains(&"done".to_string()), "{stages:?}");

    // ---- ② 预算护栏：预算 1 token，启动自动连载后立即暂停 ----
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/api/settings/pipeline")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::json!({"auto_fix_review": true, "monthly_budget_tokens": 1, "auto_batch": 10}).to_string(),
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
                .method("POST")
                .uri("/api/novels")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::json!({"idea": "预算护栏测试", "chapters": 1}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let n2 = json_body(resp).await["novel_id"].as_str().unwrap().to_string();
    wait_done(app.clone(), n2.clone()).await;

    // 调大目标 → 启动自动连载 → 预算已耗尽（首个检查点即停）
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PATCH")
                .uri(format!("/api/novels/{n2}"))
                .header("content-type", "application/json")
                .body(Body::from(serde_json::json!({"target_chapters": 2}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let resp = app
        .clone()
        .oneshot(Request::builder().method("POST").uri(format!("/api/novels/{n2}/auto")).body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    // 等待：批次运行因预算提前收尾 → 自动连载关闭
    let mut stopped = false;
    for _ in 0..400 {
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        let resp = app
            .clone()
            .oneshot(Request::builder().uri(format!("/api/novels/{n2}")).body(Body::empty()).unwrap())
            .await
            .unwrap();
        let v = json_body(resp).await;
        if !v["novel"]["auto_continue"].as_bool().unwrap_or(true) {
            stopped = true;
            break;
        }
    }
    assert!(stopped, "预算耗尽后自动连载应自动暂停");
    let stages2: Vec<String> = engine.hub.history(&n2)
        .iter()
        .filter_map(|e| match e {
            EngineEvent::Stage { stage, .. } => Some(stage.clone()),
            _ => None,
        })
        .collect();
    assert!(stages2.contains(&"budget".to_string()), "应有预算护栏事件: {stages2:?}");

    // 月度用量接口
    let resp = app
        .clone()
        .oneshot(Request::builder().uri("/api/usage").body(Body::empty()).unwrap())
        .await
        .unwrap();
    let v = json_body(resp).await;
    assert!(v["used"].as_i64().unwrap_or(0) > 0);
    assert_eq!(v["over_budget"], true);

    let _ = std::fs::remove_dir_all(&dir);
}

/// 一键「AI 处理全部审校」：待处理计数 → 后台处理 → 完成事件 + 修复用量
#[tokio::test]
async fn review_all_endpoint() {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("ainovel-test-reviewall-{stamp}"));
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
                    serde_json::json!({"idea": "一键审校测试", "chapters": 2}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let novel_id = json_body(resp).await["novel_id"].as_str().unwrap().to_string();

    let mut status = String::new();
    for _ in 0..900 {
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        let resp = app
            .clone()
            .oneshot(Request::builder().uri(format!("/api/novels/{novel_id}")).body(Body::empty()).unwrap())
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

    // Mock 审校固定留 1 条 medium → 两章均为「待处理」
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/novels/{novel_id}/review-all"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let v = json_body(resp).await;
    assert_eq!(v["queued"], 2, "{v}");

    // 等待后台处理完成事件
    let mut finished = false;
    for _ in 0..900 {
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        let done = engine.hub.history(&novel_id).iter().any(|e| match e {
            EngineEvent::Stage { stage, message } => {
                stage == "done" && message.contains("审校处理完成")
            }
            _ => false,
        });
        if done {
            finished = true;
            break;
        }
    }
    assert!(finished, "未等到审校处理完成事件");

    // 用量：流水线每章 1 次 + 一键处理每章 1 次 ≥ 4
    let resp = app
        .clone()
        .oneshot(Request::builder().uri(format!("/api/novels/{novel_id}/usage")).body(Body::empty()).unwrap())
        .await
        .unwrap();
    let u = json_body(resp).await;
    let fix_calls = u["by_task"]
        .as_array()
        .unwrap()
        .iter()
        .find(|g| g["key"] == "auto_fix")
        .map(|g| g["calls"].as_i64().unwrap_or(0))
        .unwrap_or(0);
    assert!(fix_calls >= 4, "auto_fix calls = {fix_calls}: {u}");

    // 伏笔自动审核：流水线收尾 1 次 + 一键处理阶段二 1 次
    let audit_calls = u["by_task"]
        .as_array()
        .unwrap()
        .iter()
        .find(|g| g["key"] == "foreshadow_audit")
        .map(|g| g["calls"].as_i64().unwrap_or(0))
        .unwrap_or(0);
    assert!(audit_calls >= 2, "foreshadow_audit calls = {audit_calls}: {u}");

    // 审校记录仍在，且每条意见都带生命周期状态（open/fixed）
    let resp = app
        .clone()
        .oneshot(Request::builder().uri(format!("/api/novels/{novel_id}/chapters/1")).body(Body::empty()).unwrap())
        .await
        .unwrap();
    let v = json_body(resp).await;
    assert!(v["review"].is_string(), "章节应保留审校记录");
    let review: serde_json::Value =
        serde_json::from_str(v["review"].as_str().unwrap()).unwrap();
    let issues = review["issues"].as_array().unwrap();
    assert!(!issues.is_empty(), "mock 审校应留意见: {review}");
    assert!(
        issues
            .iter()
            .all(|i| i["status"] == "open" || i["status"] == "fixed"),
        "每条意见都要有状态: {review}"
    );
    assert!(
        issues.iter().any(|i| i["status"] == "open"),
        "mock 复核问题未减少，应仍为 open: {review}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// 等一次运行完成（mock 流水线通用）
async fn wait_run_done(app: axum::Router, novel_id: &str) {
    for _ in 0..900 {
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
        let status = v["run"]["status"].as_str().unwrap_or("");
        if status == "done" {
            return;
        }
        assert_ne!(status, "failed", "run failed: {v}");
    }
    panic!("run 未在时限内完成");
}

/// 全文检索 + 作品总纲字段 + 列表瘦身（v0.8.0）
#[tokio::test]
async fn chapter_search_and_novel_meta() {
    fn pct(s: &str) -> String {
        s.bytes()
            .map(|b| {
                if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
                    (b as char).to_string()
                } else {
                    format!("%{b:02X}")
                }
            })
            .collect()
    }
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("ainovel-test-search-{stamp}"));
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
                    serde_json::json!({"idea": "全文检索测试", "chapters": 2}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let novel_id = json_body(resp).await["novel_id"].as_str().unwrap().to_string();
    wait_run_done(app.clone(), &novel_id).await;

    // 列表接口瘦身：不含 content，但有 preview / words；总纲含世界观
    let resp = app
        .clone()
        .oneshot(Request::builder().uri(format!("/api/novels/{novel_id}")).body(Body::empty()).unwrap())
        .await
        .unwrap();
    let v = json_body(resp).await;
    let ch0 = &v["chapters"][0];
    assert!(ch0.get("content").is_none(), "列表不应返回正文：{ch0}");
    assert!(!ch0["preview"].as_str().unwrap_or("").is_empty(), "{ch0}");
    assert!(ch0["words"].as_i64().unwrap_or(0) > 0, "{ch0}");
    assert!(
        !v["novel"]["worldview"].as_str().unwrap_or("").is_empty(),
        "立项应生成世界观：{v}"
    );

    // 检索命中：两章 mock 正文都含「模拟模式」
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/api/novels/{novel_id}/search?q={}", pct("模拟模式")))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let v = json_body(resp).await;
    let items = v["items"].as_array().unwrap();
    assert!(items.len() >= 2, "{v}");
    assert!(
        items
            .iter()
            .all(|it| it["excerpt"].as_str().unwrap_or("").contains("模拟")),
        "摘录应包含命中词：{v}"
    );

    // 无命中
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/api/novels/{novel_id}/search?q={}", pct("不存在词组xyz")))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let v = json_body(resp).await;
    assert_eq!(v["items"].as_array().unwrap().len(), 0, "{v}");

    // 写作偏好回环：伏笔审核间隔
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/api/settings/pipeline")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "auto_fix_review": true,
                        "monthly_budget_tokens": 0,
                        "auto_batch": 10,
                        "foreshadow_audit_every": 5
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let resp = app
        .clone()
        .oneshot(Request::builder().uri("/api/settings/pipeline").body(Body::empty()).unwrap())
        .await
        .unwrap();
    let v = json_body(resp).await;
    assert_eq!(v["foreshadow_audit_every"], 5, "{v}");

    let _ = std::fs::remove_dir_all(&dir);
}

/// 停止按钮：请求停止进行中的运行 → 运行暂停（已写章节保留）→ 可断点续写
#[tokio::test]
async fn stop_novel_interrupts_run_and_can_resume() {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("ainovel-test-stop-{stamp}"));
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
                    serde_json::json!({"idea": "停止测试", "chapters": 50}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let novel_id = json_body(resp).await["novel_id"].as_str().unwrap().to_string();

    // 立刻请求停止；mock 很快时可能来不及命中 running（两种路径都接受，不误报）
    let mut stopping = false;
    for _ in 0..600 {
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/novels/{novel_id}/stop"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let v = json_body(resp).await;
        if v["stopping"].as_bool().unwrap_or(false) {
            stopping = true;
            break;
        }
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
        if v["run"]["status"].as_str().unwrap_or("") != "running" {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }

    // 等运行落定
    for _ in 0..900 {
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
        if v["run"]["status"].as_str().unwrap_or("") != "running" {
            break;
        }
    }
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
    let status = v["run"]["status"].as_str().unwrap_or("").to_string();
    assert!(
        status == "paused" || status == "done",
        "停止后运行应落定为 paused/done：{v}"
    );
    assert!(
        !v["novel"]["auto_continue"].as_bool().unwrap_or(true),
        "停止请求应关闭自动连载：{v}"
    );

    if stopping {
        assert_eq!(status, "paused", "收到停止请求后运行应为 paused：{v}");
        assert_eq!(v["run"]["stage"], "stopped", "{v}");
        let msgs: Vec<String> = engine
            .hub
            .history(&novel_id)
            .iter()
            .filter_map(|e| match e {
                EngineEvent::RunPaused { message, .. } => Some(message.clone()),
                _ => None,
            })
            .collect();
        assert!(
            msgs.iter().any(|m| m.contains("停止")),
            "应有停止的 RunPaused 提示：{msgs:?}"
        );
        let done: i64 = v["chapters"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|c| c["status"] == "done")
            .count() as i64;
        assert!(done < 50, "应确实中断而非全部写完：{done}");

        // 断点续写：停止标记已清除，可继续手动创作并正常完成
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/novels/{novel_id}/run"))
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::json!({"chapters": 2}).to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        wait_run_done(app.clone(), &novel_id).await;
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
        assert_eq!(v["run"]["status"], "done", "断点续写应正常完成：{v}");
    }

    let _ = std::fs::remove_dir_all(&dir);
}

/// 并发守卫：有运行进行中时不能恢复备份 / 启动审校 / 再次运行；审校处理进行中时不能重复入队
#[tokio::test]
async fn restore_and_review_concurrency_guards() {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("ainovel-test-guards-{stamp}"));
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
                    serde_json::json!({"idea": "守卫测试", "chapters": 1}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let novel_id = json_body(resp).await["novel_id"].as_str().unwrap().to_string();
    wait_run_done(app.clone(), &novel_id).await;

    // 制造一个「进行中」的运行（不跑流水线，仅占位）
    engine.db.create_run(&novel_id, 3).unwrap();
    assert!(engine.db.any_running_run().unwrap().is_some());

    // 恢复备份 → 409（不能把数据写进正在运行的库）
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/restore")
                .body(Body::from(b"whatever".to_vec()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CONFLICT, "运行中恢复备份应被拒绝");

    // 再次发起创作 → 409
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/novels/{novel_id}/run"))
                .header("content-type", "application/json")
                .body(Body::from(serde_json::json!({"chapters": 1}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CONFLICT);

    // 结束占位运行
    let run = engine.db.latest_run(&novel_id).unwrap().unwrap();
    engine.db.update_run(&run.id, "done", "done", 0, None).unwrap();

    // 非法备份但无任务在跑 → 400（前置检查已放行，错误来自 zip 解析）
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/restore")
                .body(Body::from(b"not-a-zip".to_vec()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

    // 审校处理进行中：重复入队 → 409；释放后可入队
    assert!(engine.begin_review(&novel_id));
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/novels/{novel_id}/review-all"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CONFLICT, "重复审校处理应被拒绝");
    engine.end_review(&novel_id);
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/novels/{novel_id}/review-all"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let _ = std::fs::remove_dir_all(&dir);
}

/// 章节历史版本（留档/复原/AI 复原）+ 全局审校接口 + 新设置项回环
#[tokio::test]
async fn version_history_and_global_review_endpoints() {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("ainovel-test-versions-{stamp}"));
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
                    serde_json::json!({"idea": "版本与全局审校测试", "chapters": 3}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let novel_id = json_body(resp).await["novel_id"].as_str().unwrap().to_string();
    wait_run_done(app.clone(), &novel_id).await;

    // 记下原始正文
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
    let original = json_body(resp).await["content"].as_str().unwrap().to_string();
    assert!(!original.is_empty());

    // 连续两次手动编辑：每次覆盖前自动留档（pre-manual）
    for content in ["第一次改写，内容较长以便对比评分。", "第二次改写，更短。"] {
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri(format!("/api/novels/{novel_id}/chapters/1"))
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::json!({"content": content}).to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/api/novels/{novel_id}/chapters/1/versions"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let v = json_body(resp).await;
    let items = v["items"].as_array().unwrap();
    assert!(items.len() >= 2, "应有至少两次留档：{v}");
    assert!(
        items.iter().all(|it| it["source"].as_str().unwrap_or("").starts_with("pre-")),
        "{v}"
    );
    // 每个版本都带综合分与字数（供 UI 标「更优」）
    assert!(items[0]["score"].is_i64() && items[0]["words"].as_i64().unwrap_or(0) > 0, "{v}");

    // 手动复原最旧的一版 → 正文回到原始稿
    let oldest = items.last().unwrap()["id"].as_str().unwrap().to_string();
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/novels/{novel_id}/chapters/1/versions/{oldest}/restore"))
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
                .uri(format!("/api/novels/{novel_id}/chapters/1"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let v = json_body(resp).await;
    assert_eq!(v["content"].as_str().unwrap(), original, "复原应拿回原始稿");

    // 当前稿已是最优 → AI 复原应报告「无需复原」
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/novels/{novel_id}/chapters/1/ai-restore"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let v = json_body(resp).await;
    assert_eq!(v["restored"], false, "{v}");

    // 制造更差版本（极短稿）：AI 复原应自动恢复更优快照
    engine
        .db
        .save_chapter_content_sourced(&novel_id, 1, "极短稿。", "done", "manual")
        .unwrap();
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/novels/{novel_id}/chapters/1/ai-restore"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let v = json_body(resp).await;
    assert_eq!(v["restored"], true, "应自动恢复更优快照：{v}");
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
    assert_ne!(v["content"].as_str().unwrap(), "极短稿。", "更短稿应被替换");

    // 全局审校：手动触发 → 等后台完成（用量里出现 global_review）→ 读取结论
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/novels/{novel_id}/global-review"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let mut saw_call = false;
    for _ in 0..400 {
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/api/novels/{novel_id}/usage"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let u = json_body(resp).await;
        if u["by_task"]
            .as_array()
            .unwrap()
            .iter()
            .any(|g| g["key"] == "global_review")
        {
            saw_call = true;
            break;
        }
    }
    assert!(saw_call, "全局审校应产生一次 LLM 调用");
    // 等槽位释放后再读结论（mock 很快）
    for _ in 0..200 {
        if !engine.is_reviewing(&novel_id) {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/api/novels/{novel_id}/global-review"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let v = json_body(resp).await;
    assert_eq!(v["ok"], true, "{v}");
    assert!(
        !v["updated_at"].as_str().unwrap_or("").is_empty(),
        "应有检查时间：{v}"
    );

    // 新设置项回环
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/api/settings/pipeline")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "auto_fix_review": true,
                        "monthly_budget_tokens": 0,
                        "auto_batch": 10,
                        "foreshadow_audit_every": 1,
                        "global_review_every": 5,
                        "auto_fix_global": false
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let resp = app
        .clone()
        .oneshot(Request::builder().uri("/api/settings/pipeline").body(Body::empty()).unwrap())
        .await
        .unwrap();
    let v = json_body(resp).await;
    assert_eq!(v["global_review_every"], 5, "{v}");
    assert_eq!(v["auto_fix_global"], false, "{v}");

    let _ = std::fs::remove_dir_all(&dir);
}

/// 开工巡检端点：done 无正文 → 状态修正；缺摘要 → 自动补齐
#[tokio::test]
async fn maintenance_endpoint_heals_state() {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("ainovel-test-maintain-{stamp}"));
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
                    serde_json::json!({"idea": "巡检验证", "chapters": 2}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let novel_id = json_body(resp).await["novel_id"].as_str().unwrap().to_string();
    wait_run_done(app.clone(), &novel_id).await;

    // 人为破坏：章 2 正文清空但保持 done；章 1 摘要清空
    engine.db.save_chapter_content(&novel_id, 2, "", "done").unwrap();
    engine.db.set_chapter_summary(&novel_id, 1, "").unwrap();

    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/novels/{novel_id}/maintenance"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(json_body(resp).await["queued"], true);

    // 等巡检完成（章 2 状态被收回 + 章 1 摘要被补齐）
    let mut healed_status = false;
    let mut healed_summary = false;
    for _ in 0..600 {
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        let ch2 = engine.db.get_chapter(&novel_id, 2).unwrap().unwrap();
        let ch1 = engine.db.get_chapter(&novel_id, 1).unwrap().unwrap();
        healed_status = ch2.status != "done";
        healed_summary = !ch1.summary.trim().is_empty();
        if healed_status && healed_summary {
            break;
        }
    }
    assert!(healed_status, "done 无正文的章节应被收回重写状态");
    assert!(healed_summary, "缺摘要应被自动补齐");
    // 巡检消息可见（供 UI 日志）
    let msgs: Vec<String> = engine
        .hub
        .history(&novel_id)
        .iter()
        .filter_map(|e| match e {
            EngineEvent::Stage { message, .. } => Some(message.clone()),
            _ => None,
        })
        .collect();
    assert!(
        msgs.iter().any(|m| m.contains("开工巡检")),
        "应有开工巡检事件：{msgs:?}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// 批量删除 + 从某章之后重写（自动启动重写运行，旧稿留档可回滚）
#[tokio::test]
async fn batch_delete_and_rewrite_after_endpoints() {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("ainovel-test-batch-{stamp}"));
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
                    serde_json::json!({"idea": "批量操作测试", "chapters": 3}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let novel_id = json_body(resp).await["novel_id"].as_str().unwrap().to_string();
    wait_run_done(app.clone(), &novel_id).await;

    // ① 从第 1 章之后重写（章纲保留）：2~3 章清空后自动重写
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/novels/{novel_id}/chapters/rewrite-after"))
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::json!({"after_idx": 1, "replan": false}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let v = json_body(resp).await;
    assert_eq!(v["count"], 2, "{v}");
    assert_eq!(v["from"], 2, "{v}");
    // 等重写运行完成
    wait_run_done(app.clone(), &novel_id).await;
    let ch2 = engine.db.get_chapter(&novel_id, 2).unwrap().unwrap();
    assert!(!ch2.content.trim().is_empty() && ch2.status == "done", "第 2 章应被重新写好");
    // 旧稿留档（可回滚）
    let versions = engine.db.list_chapter_versions(&novel_id, 2).unwrap();
    assert!(
        versions.iter().any(|x| x.source == "pre-rewrite-after" && !x.content.trim().is_empty()),
        "重写前旧稿应留档：{versions:?}"
    );

    // ② 边界：末尾之后没有章节 → 400
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/novels/{novel_id}/chapters/rewrite-after"))
                .header("content-type", "application/json")
                .body(Body::from(serde_json::json!({"after_idx": 3}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

    // ③ 批量删除前：造派生数据（卷摘要 / 伏笔埋设第3章 / 全局审校涉及第3章），验证删除后引用同步
    engine
        .db
        .upsert_volume_summary(&novel_id, 1, 3, "第1卷摘要")
        .unwrap();
    engine
        .db
        .absorb_foreshadowings(
            &novel_id,
            3,
            &serde_json::json!({"foreshadowings_planted": [{"title": "跨章伏笔"}]}),
        )
        .unwrap();
    engine
        .db
        .save_global_review(
            &novel_id,
            &GlobalReview {
                ok: true,
                issues: vec![GlobalIssue {
                    severity: "medium".into(),
                    note: "第3章节奏拖沓".into(),
                    affected_chapters: vec![3],
                    suggestion: "收紧".into(),
                    status: "open".into(),
                    is_new: false,
                }],
                updated_at: "t".into(),
            },
        )
        .unwrap();

    // ④ 批量删除 2~3 → 只剩 1 章
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/novels/{novel_id}/chapters/batch-delete"))
                .header("content-type", "application/json")
                .body(Body::from(serde_json::json!({"from": 2, "to": 3}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(json_body(resp).await["deleted"], 2);
    let metas = engine.db.list_chapter_metas(&novel_id).unwrap();
    assert_eq!(metas.len(), 1, "应只剩第 1 章");

    // 派生数据已同步：伏笔埋设章号 3（被删）→ 落到其前最近存活章 1；
    // 全局审校涉及章节 → [1]；失效卷摘要被清理
    let fs = engine.db.list_foreshadowings(&novel_id).unwrap();
    let f = fs.iter().find(|f| f.title == "跨章伏笔").unwrap();
    assert_eq!(f.planted_chapter, 1, "被删章号的引用应就近落到存活章：{f:?}");
    let gr = engine.db.get_global_review(&novel_id).unwrap().unwrap();
    assert_eq!(gr.issues[0].affected_chapters, vec![1], "{gr:?}");
    assert!(
        engine.db.list_volume_summaries(&novel_id).unwrap().is_empty(),
        "失效卷摘要应被清理"
    );

    // ⑤ 保底：不能全删 → 400
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/novels/{novel_id}/chapters/batch-delete"))
                .header("content-type", "application/json")
                .body(Body::from(serde_json::json!({"from": 1, "to": 1}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

    let _ = std::fs::remove_dir_all(&dir);
}

/// 立项澄清：AI 提问 → 带答案建书 → 问答入库（作为「作者已确认的设定」）
#[tokio::test]
async fn clarify_then_create_with_answers() {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("ainovel-test-clarify-{stamp}"));
    let engine = Engine::new(AppConfig::for_data_dir(dir.clone())).unwrap();
    engine.db.set_llm_settings(&LlmSettings::default()).unwrap();
    let app = api::build_router(engine.clone());

    // ① 空 idea → 400
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/clarify")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::json!({"idea": "   "}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

    // ② 提问：mock 返回 3 个问题（含建议选项）
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/clarify")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::json!({"idea": "一个能听见鲸歌的少年"}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let v = json_body(resp).await;
    let questions = v["questions"].as_array().unwrap();
    assert!(questions.len() >= 2, "{v}");
    assert!(!questions[0]["question"].as_str().unwrap().is_empty(), "{v}");
    assert!(questions[0]["suggestions"].as_array().unwrap().len() >= 2, "{v}");

    // ③ 带答案建书（一条作答、一条空答案应被忽略）
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/novels")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "idea": "一个能听见鲸歌的少年",
                        "chapters": 1,
                        "clarify": [
                            {"question": "主角的性别与身份是什么？", "answer": "女性·港口调音师"},
                            {"question": "（未回答的问题）", "answer": ""}
                        ]
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let novel_id = json_body(resp).await["novel_id"].as_str().unwrap().to_string();
    wait_run_done(app.clone(), &novel_id).await;

    // ④ 问答入库：只保留已作答的
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
    let raw = v["novel"]["clarify_json"].as_str().unwrap_or("");
    assert!(raw.contains("女性·港口调音师"), "{raw}");
    assert!(!raw.contains("未回答的问题"), "空答案不应入库：{raw}");

    let _ = std::fs::remove_dir_all(&dir);
}

/// 滚动规划窗口：单次只规划一小段，写到窗口尾部前自动续规划（不再一次规划整批）
#[tokio::test]
async fn rolling_plan_window_plans_in_segments() {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("ainovel-test-rolling-{stamp}"));
    let engine = Engine::new(AppConfig::for_data_dir(dir.clone())).unwrap();
    engine.db.set_llm_settings(&LlmSettings::default()).unwrap();
    // 每批 12 章、单次规划 4 章 → 一次运行内应规划 3 段（1 段起始 + 2 段续规划）
    engine
        .db
        .set_pipeline_settings(&PipelineSettings {
            plan_window: 4,
            auto_batch: 12,
            ..PipelineSettings::default()
        })
        .unwrap();
    let app = api::build_router(engine.clone());

    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/novels")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::json!({"idea": "滚动规划测试", "chapters": 12}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let novel_id = json_body(resp).await["novel_id"].as_str().unwrap().to_string();
    wait_run_done(app.clone(), &novel_id).await;

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
    let chapters = v["chapters"].as_array().unwrap();
    let done = chapters.iter().filter(|c| c["status"] == "done").count();
    assert_eq!(done, 12, "应写完首批 12 章：{v}");

    // 节奏字段（钩型/强度）应随章纲落库并出现在章节列表
    let first = &chapters[0];
    assert!(
        !first["hook_type"].as_str().unwrap_or("").is_empty(),
        "章纲应带钩型：{first}"
    );
    let intensity = first["intensity"].as_i64().unwrap_or(0);
    assert!((1..=5).contains(&intensity), "强度应在 1-5：{first}");

    // 规划段数：1 次「生成章节规划」+ 2 次「续规划」（4 章一段）
    let msgs: Vec<String> = engine
        .hub
        .history(&novel_id)
        .iter()
        .filter_map(|e| match e {
            EngineEvent::Stage { stage, message } if stage == "outline" => Some(message.clone()),
            _ => None,
        })
        .collect();
    assert!(msgs.iter().any(|m| m == "生成章节规划…"), "{msgs:?}");
    let continued: Vec<&String> = msgs.iter().filter(|m| m.starts_with("续规划")).collect();
    assert_eq!(continued.len(), 2, "应续规划两次（每段 4 章）：{msgs:?}");
    assert!(msgs.iter().any(|m| m.contains("5~8")), "{msgs:?}");
    assert!(msgs.iter().any(|m| m.contains("9~12")), "{msgs:?}");

    let _ = std::fs::remove_dir_all(&dir);
}

/// 「继续创作」按批推进：首批 = min(目标, 批)；再点从已写处 +批（修复 50 章封顶）
#[tokio::test]
async fn manual_continue_advances_one_batch_from_progress() {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("ainovel-test-batch-{stamp}"));
    let engine = Engine::new(AppConfig::for_data_dir(dir.clone())).unwrap();
    engine.db.set_llm_settings(&LlmSettings::default()).unwrap();
    engine
        .db
        .set_pipeline_settings(&PipelineSettings {
            auto_batch: 5,
            ..PipelineSettings::default()
        })
        .unwrap();
    let app = api::build_router(engine.clone());

    // 目标 12 章 → 首批只写 5 章
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/novels")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::json!({"idea": "批量语义测试", "chapters": 12}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let v = json_body(resp).await;
    assert_eq!(v["first_batch"], 5, "首批应受每批章节数约束：{v}");
    let novel_id = v["novel_id"].as_str().unwrap().to_string();
    wait_run_done(app.clone(), &novel_id).await;

    let done_count = |app: axum::Router, novel_id: String| async move {
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
        v["chapters"]
            .as_array()
            .map(|a| a.iter().filter(|c| c["status"] == "done").count())
            .unwrap_or(0)
    };
    assert_eq!(done_count(app.clone(), novel_id.clone()).await, 5, "首批应只写 5 章");

    // 继续创作（不带 chapters）→ 写到第 10 章
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/novels/{novel_id}/run"))
                .header("content-type", "application/json")
                .body(Body::from("{}"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let v = json_body(resp).await;
    assert_eq!(v["batch_until"], 10, "第二批应写到第 10 章：{v}");
    wait_run_done(app.clone(), &novel_id).await;
    assert_eq!(done_count(app.clone(), novel_id.clone()).await, 10);

    // 再点一次 → 封顶到目标 12
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/novels/{novel_id}/run"))
                .header("content-type", "application/json")
                .body(Body::from("{}"))
                .unwrap(),
        )
        .await
        .unwrap();
    let v = json_body(resp).await;
    assert_eq!(v["batch_until"], 12, "第三批应封顶到目标：{v}");
    wait_run_done(app.clone(), &novel_id).await;
    assert_eq!(done_count(app.clone(), novel_id.clone()).await, 12);

    // 已达标再点 → 不写新章（批次终点仍为 12）
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/novels/{novel_id}/run"))
                .header("content-type", "application/json")
                .body(Body::from("{}"))
                .unwrap(),
        )
        .await
        .unwrap();
    let v = json_body(resp).await;
    assert_eq!(v["batch_until"], 12, "达标后终点不再增长：{v}");
    wait_run_done(app.clone(), &novel_id).await;
    assert_eq!(done_count(app.clone(), novel_id.clone()).await, 12);

    let _ = std::fs::remove_dir_all(&dir);
}

/// 质量体检接口：文体统计（本地零 token）+ 读者视角评审（默认黄金三章）
#[tokio::test]
async fn health_extras_style_stats_and_reader_review() {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("ainovel-test-health-{stamp}"));
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
                    serde_json::json!({"idea": "质量体检测试", "chapters": 3}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let novel_id = json_body(resp).await["novel_id"].as_str().unwrap().to_string();
    wait_run_done(app.clone(), &novel_id).await;

    // 文体统计：mock 正文含跨章重复句，应被检出
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/api/novels/{novel_id}/style-stats"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let v = json_body(resp).await;
    assert_eq!(v["chapters"], 3, "{v}");
    assert!(
        !v["summary_text"].as_str().unwrap_or("").is_empty(),
        "{v}"
    );
    assert!(
        !v["repeated_sentences"].as_array().unwrap().is_empty(),
        "mock 正文的跨章重复句应被检出：{v}"
    );

    // 读者视角评审（默认 1~3 章 → 黄金三章专项）
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/novels/{novel_id}/reader-review"))
                .header("content-type", "application/json")
                .body(Body::from(serde_json::json!({"from": 1, "to": 3}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let v = json_body(resp).await;
    assert_eq!(v["golden"], true, "{v}");
    assert!(v["result"]["overall_score"].is_number(), "{v}");
    assert!(
        !v["result"]["chapters"].as_array().unwrap().is_empty(),
        "{v}"
    );

    // 区间没有正文 → 400
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/novels/{novel_id}/reader-review"))
                .header("content-type", "application/json")
                .body(Body::from(serde_json::json!({"from": 50, "to": 60}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

    // 未满一卷 → 卷末体检报告为空
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/api/novels/{novel_id}/volume-reports"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert!(json_body(resp).await["items"].as_array().unwrap().is_empty());

    let _ = std::fs::remove_dir_all(&dir);
}

/// 卷末体检：写满第 20 章（一卷）自动生成报告，体检接口可读
#[tokio::test]
async fn volume_review_generated_at_volume_boundary() {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("ainovel-test-volrev-{stamp}"));
    let engine = Engine::new(AppConfig::for_data_dir(dir.clone())).unwrap();
    engine.db.set_llm_settings(&LlmSettings::default()).unwrap();
    engine
        .db
        .set_pipeline_settings(&PipelineSettings {
            auto_batch: 20,
            ..PipelineSettings::default()
        })
        .unwrap();
    let app = api::build_router(engine.clone());

    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/novels")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::json!({"idea": "卷末体检测试", "chapters": 20}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let novel_id = json_body(resp).await["novel_id"].as_str().unwrap().to_string();
    wait_run_done(app.clone(), &novel_id).await;

    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/api/novels/{novel_id}/volume-reports"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let v = json_body(resp).await;
    let items = v["items"].as_array().unwrap();
    assert_eq!(items.len(), 1, "写满一卷应生成一条体检报告：{v}");
    assert_eq!(items[0]["volume_idx"], 1, "{v}");
    assert_eq!(items[0]["upto_chapter"], 20, "{v}");
    assert!(
        !items[0]["report"]["findings"].as_array().unwrap().is_empty(),
        "mock 体检应带 findings：{v}"
    );

    // 文体统计覆盖 20 章
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/api/novels/{novel_id}/style-stats"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let v = json_body(resp).await;
    assert_eq!(v["chapters"], 20, "{v}");

    let _ = std::fs::remove_dir_all(&dir);
}