use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use tower::ServiceExt;

use ainovel_engine::config::AppConfig;
use ainovel_engine::models::{ChapterPlan, LlmSettings};
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

    // 1) 目录：8 个任务，默认模板齐全，初始无覆盖
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
    assert_eq!(items.len(), 8, "任务数量应为 8");
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

    // 审校记录仍在
    let resp = app
        .clone()
        .oneshot(Request::builder().uri(format!("/api/novels/{novel_id}/chapters/1")).body(Body::empty()).unwrap())
        .await
        .unwrap();
    let v = json_body(resp).await;
    assert!(v["review"].is_string(), "章节应保留审校记录");

    let _ = std::fs::remove_dir_all(&dir);
}