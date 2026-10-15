use std::sync::Arc;

use serde::Serialize;
use tokio::sync::broadcast;

use crate::llm::{extract_json_object, Llm};
use crate::models::*;
use crate::Engine;

/// 引擎事件（SSE 推送给 UI）
#[derive(Clone, Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum EngineEvent {
    RunStarted { run_id: String, novel_id: String, chapters: i64 },
    Stage { stage: String, message: String },
    Token { chapter_idx: i64, delta: String },
    ChapterSaved { chapter_idx: i64, title: String, words: usize },
    ChapterReview { chapter_idx: i64, ok: bool, issues: usize },
    RunFinished { run_id: String },
    RunFailed { run_id: String, error: String },
}

#[derive(Clone)]
pub struct EventHub {
    tx: broadcast::Sender<EngineEvent>,
}

impl EventHub {
    pub fn new() -> Self {
        let (tx, _rx) = broadcast::channel(2048);
        Self { tx }
    }

    pub fn publish(&self, ev: EngineEvent) {
        let _ = self.tx.send(ev);
    }

    pub fn subscribe(&self) -> broadcast::Receiver<EngineEvent> {
        self.tx.subscribe()
    }
}

impl Default for EventHub {
    fn default() -> Self {
        Self::new()
    }
}

/// 创建一次运行并在后台执行；立即返回 run_id。
/// 断点续跑：重复调用同一 novel 时，已完成的章节会跳过。
pub fn spawn(engine: Arc<Engine>, novel_id: &str, chapters: i64) -> anyhow::Result<String> {
    let run = engine.db.create_run(novel_id, chapters)?;
    let run_id = run.id.clone();
    engine.db.set_novel_status(novel_id, "writing")?;
    engine.hub.publish(EngineEvent::RunStarted {
        run_id: run_id.clone(),
        novel_id: novel_id.to_string(),
        chapters,
    });
    let engine2 = engine.clone();
    let nid = novel_id.to_string();
    let rid = run_id.clone();
    tokio::spawn(async move {
        if let Err(e) = run_inner(engine2.clone(), &nid, &rid, chapters).await {
            let msg = format!("{e:#}");
            let _ = engine2.db.update_run(&rid, "failed", "failed", 0, Some(&msg));
            let _ = engine2.db.set_novel_status(&nid, "failed");
            engine2.hub.publish(EngineEvent::RunFailed { run_id: rid, error: msg });
        }
    });
    Ok(run_id)
}

async fn run_inner(
    engine: Arc<Engine>,
    novel_id: &str,
    run_id: &str,
    chapters: i64,
) -> anyhow::Result<()> {
    let settings = engine.db.llm_settings()?;
    let llm = Llm::new(settings);

    // ---- 1) 立项方案 ----
    let mut novel = engine
        .db
        .get_novel(novel_id)?
        .ok_or_else(|| anyhow::anyhow!("novel not found: {novel_id}"))?;
    if novel.title.trim().is_empty() {
        engine.hub.publish(EngineEvent::Stage {
            stage: "brief".into(),
            message: "生成立项方案…".into(),
        });
        let (sys, usr) = crate::prompts::brief_prompt(&novel.idea);
        let raw = llm.complete(&sys, &usr).await?;
        let v = extract_json_object(&raw).ok_or_else(|| {
            anyhow::anyhow!("brief 输出不是 JSON：{}", raw.chars().take(200).collect::<String>())
        })?;
        let brief = Brief {
            title: str_field(&v, "title", "未命名"),
            genre: str_field(&v, "genre", ""),
            logline: str_field(&v, "logline", ""),
            protagonist: str_field(&v, "protagonist", ""),
            style: str_field(&v, "style", ""),
        };
        engine.db.update_novel_brief(novel_id, &brief)?;
        novel = engine.db.get_novel(novel_id)?.unwrap_or(novel);
        engine.db.update_run(run_id, "running", "outline", 0, None)?;
    }

    // ---- 2) 章节规划（只补缺失章节，不覆盖已有规划）----
    let existing = engine.db.list_chapters(novel_id)?;
    let have: std::collections::BTreeSet<i64> = existing.iter().map(|c| c.idx).collect();
    let missing: Vec<i64> = (1..=chapters).filter(|i| !have.contains(i)).collect();
    if !missing.is_empty() {
        let from = missing[0];
        let to = chapters;
        engine.hub.publish(EngineEvent::Stage {
            stage: "outline".into(),
            message: if from == 1 {
                "生成章节规划…".into()
            } else {
                format!("续规划第 {from}~{to} 章…")
            },
        });
        let brief = Brief {
            title: novel.title.clone(),
            genre: novel.genre.clone(),
            logline: novel.logline.clone(),
            protagonist: novel.protagonist.clone(),
            style: novel.style.clone(),
        };
        let prev_all: Vec<(i64, String)> = existing
            .iter()
            .filter(|c| c.idx < from)
            .map(|c| (c.idx, c.title.clone()))
            .collect();
        let prev_tail = &prev_all[prev_all.len().saturating_sub(5)..];
        let (sys, usr) = crate::prompts::outline_prompt(&brief, from, to, prev_tail);
        let raw = llm.complete(&sys, &usr).await?;
        let v = extract_json_object(&raw).ok_or_else(|| {
            anyhow::anyhow!("outline 输出不是 JSON：{}", raw.chars().take(200).collect::<String>())
        })?;
        if novel.book_outline.trim().is_empty() {
            let book_outline = str_field(&v, "book_outline", "");
            engine.db.update_novel_outline(novel_id, &book_outline, "outlined")?;
        }
        if let Some(arr) = v["chapters"].as_array() {
            for item in arr {
                let idx = item["idx"].as_i64().unwrap_or(0);
                if idx < from || idx > to {
                    continue;
                }
                let plan = ChapterPlan {
                    idx,
                    title: str_field(item, "title", &format!("第{idx}章")),
                    goal: str_field(item, "goal", ""),
                    beats: item["beats"]
                        .as_array()
                        .map(|a| {
                            a.iter()
                                .filter_map(|x| x.as_str().map(str::to_string))
                                .collect()
                        })
                        .unwrap_or_default(),
                };
                engine.db.upsert_chapter_plan(novel_id, &plan)?;
            }
        }
        engine.db.update_run(run_id, "running", "draft", 0, None)?;
    }
    let novel = engine.db.get_novel(novel_id)?.unwrap_or(novel);

    // ---- 3) 逐章：写正文 + 审校 ----
    for idx in 1..=chapters {
        if engine.db.get_novel(novel_id)?.is_none() {
            // 作品已被删除：静默结束本次运行
            return Ok(());
        }
        let ch = match engine.db.get_chapter(novel_id, idx)? {
            Some(c) => c,
            None => {
                let plan = ChapterPlan {
                    idx,
                    title: format!("第{idx}章"),
                    goal: String::new(),
                    beats: Vec::new(),
                };
                engine.db.upsert_chapter_plan(novel_id, &plan)?;
                engine
                    .db
                    .get_chapter(novel_id, idx)?
                    .ok_or_else(|| anyhow::anyhow!("chapter row missing after upsert"))?
            }
        };
        if ch.status == "done" && !ch.content.trim().is_empty() {
            // 断点续跑：该章已完成
            engine.db.update_run(run_id, "running", "draft", idx, None)?;
            continue;
        }

        let prev_tail = if idx > 1 {
            engine
                .db
                .get_chapter(novel_id, idx - 1)?
                .map(|c| tail_chars(&c.content, 400))
                .unwrap_or_default()
        } else {
            String::new()
        };

        engine.hub.publish(EngineEvent::Stage {
            stage: "draft".into(),
            message: format!("正在写第 {idx} 章…"),
        });
        let brief = Brief {
            title: novel.title.clone(),
            genre: novel.genre.clone(),
            logline: novel.logline.clone(),
            protagonist: novel.protagonist.clone(),
            style: novel.style.clone(),
        };
        let plan = ChapterPlan {
            idx: ch.idx,
            title: ch.title.clone(),
            goal: ch.goal.clone(),
            beats: ch.beats.clone(),
        };
        let (sys, usr) = crate::prompts::chapter_prompt(&brief, &novel.book_outline, &plan, &prev_tail);
        // 流式输出按 ~80 字聚合后再推送，降低 SSE 事件与前端渲染压力
        let mut buf = String::new();
        let hub = engine.hub.clone();
        let content = llm
            .stream(&sys, &usr, |delta: &str| {
                buf.push_str(delta);
                if buf.chars().count() >= 80 {
                    let chunk = std::mem::take(&mut buf);
                    hub.publish(EngineEvent::Token {
                        chapter_idx: idx,
                        delta: chunk,
                    });
                }
            })
            .await?;
        if !buf.is_empty() {
            let chunk = std::mem::take(&mut buf);
            engine.hub.publish(EngineEvent::Token {
                chapter_idx: idx,
                delta: chunk,
            });
        }
        let content = content.trim().to_string();
        if content.is_empty() {
            anyhow::bail!("第 {idx} 章生成内容为空");
        }
        engine.db.save_chapter_content(novel_id, idx, &content, "drafted")?;
        engine.hub.publish(EngineEvent::ChapterSaved {
            chapter_idx: idx,
            title: ch.title.clone(),
            words: content.chars().count(),
        });

        // 审校：非阻塞 —— 失败只记录日志，不中断生产链
        let (rsys, rusr) = crate::prompts::review_prompt(&content);
        match llm.complete(&rsys, &rusr).await {
            Ok(raw) => {
                if let Some(j) = extract_json_object(&raw) {
                    let review = Review {
                        ok: j["ok"].as_bool().unwrap_or(true),
                        issues: j["issues"]
                            .as_array()
                            .map(|a| {
                                a.iter()
                                    .map(|it| ReviewIssue {
                                        severity: str_field(it, "severity", "info"),
                                        note: str_field(it, "note", ""),
                                        quote: str_field(it, "quote", ""),
                                    })
                                    .collect()
                            })
                            .unwrap_or_default(),
                    };
                    engine.db.save_chapter_review(novel_id, idx, &review)?;
                    engine.hub.publish(EngineEvent::ChapterReview {
                        chapter_idx: idx,
                        ok: review.ok,
                        issues: review.issues.len(),
                    });
                }
            }
            Err(e) => tracing::warn!("chapter {idx} review failed: {e:#}"),
        }

        engine.db.save_chapter_content(novel_id, idx, &content, "done")?;
        engine.db.update_run(run_id, "running", "draft", idx, None)?;
    }

    engine.db.update_run(run_id, "done", "done", chapters, None)?;
    engine.db.set_novel_status(novel_id, "drafted")?;
    engine.hub.publish(EngineEvent::RunFinished {
        run_id: run_id.to_string(),
    });
    Ok(())
}

fn str_field(v: &serde_json::Value, key: &str, default: &str) -> String {
    v[key]
        .as_str()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| default.to_string())
}

fn tail_chars(s: &str, n: usize) -> String {
    let total = s.chars().count();
    if total <= n {
        s.to_string()
    } else {
        s.chars().skip(total - n).collect()
    }
}
