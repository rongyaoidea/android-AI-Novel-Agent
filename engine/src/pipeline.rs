use std::sync::Arc;

use serde::Serialize;
use tokio::sync::broadcast;

use crate::llm::{extract_json_object, record_usage, Llm};
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
    LedgerUpdated { chapter_idx: i64, characters: usize, foreshadowings: usize },
    RunPaused { run_id: String, novel_id: String, message: String },
    RunFinished { run_id: String },
    RunFailed { run_id: String, error: String },
}

/// 事件中心：广播给 SSE 订阅者 + 每书保留最近 N 条关键事件的环形缓冲
/// （刷新页面后重连可回放到日志面板；Token 流不进入缓冲）
#[derive(Clone)]
pub struct EventHub {
    tx: broadcast::Sender<EngineEvent>,
    history: std::sync::Arc<
        std::sync::Mutex<std::collections::HashMap<String, std::collections::VecDeque<EngineEvent>>>,
    >,
}

const HISTORY_CAPACITY: usize = 300;
const HISTORY_MAX_NOVELS: usize = 200;

impl EventHub {
    pub fn new() -> Self {
        let (tx, _rx) = broadcast::channel(2048);
        Self {
            tx,
            history: std::sync::Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        }
    }

    pub fn publish(&self, novel_id: &str, ev: EngineEvent) {
        // 关键事件进历史（Token 是流式增量，不入缓冲）
        if !matches!(ev, EngineEvent::Token { .. }) {
            if let Ok(mut h) = self.history.lock() {
                if h.len() > HISTORY_MAX_NOVELS && !h.contains_key(novel_id) {
                    if let Some(k) = h.keys().next().cloned() {
                        h.remove(&k);
                    }
                }
                let q = h.entry(novel_id.to_string()).or_default();
                if q.len() >= HISTORY_CAPACITY {
                    q.pop_front();
                }
                q.push_back(ev.clone());
            }
        }
        let _ = self.tx.send(ev);
    }

    /// 某本书的关键事件历史（用于 SSE 重连回放）
    pub fn history(&self, novel_id: &str) -> Vec<EngineEvent> {
        self.history
            .lock()
            .ok()
            .and_then(|h| h.get(novel_id).map(|q| q.iter().cloned().collect()))
            .unwrap_or_default()
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

/// 自动连载：每批章节数（批间有运行检查点，便于崩溃恢复与进度可见）
pub const AUTO_BATCH: i64 = 10;

/// 自动连载的下一批目标截止章号 = min(已规划最大章号 + 每批章数, 全书目标)
pub fn auto_batch_target(
    db: &crate::db::Db,
    novel_id: &str,
    target: i64,
    batch: i64,
) -> anyhow::Result<i64> {
    let chapters = db.list_chapters(novel_id)?;
    let max_idx = chapters.iter().map(|c| c.idx).max().unwrap_or(0);
    Ok((max_idx + batch.clamp(1, 50)).min(target.max(0)))
}

/// 运行成功收尾：若开启自动连载且未达目标且未超预算 → 自动启动下一批。
/// 停止条件：目标达成（自动关闭）/ 预算耗尽（自动关闭并提示）/ 手动关闭。
fn maybe_chain_auto(engine: &Arc<Engine>, novel_id: &str) -> anyhow::Result<()> {
    let Some(novel) = engine.db.get_novel(novel_id)? else {
        return Ok(());
    };
    if !novel.auto_continue {
        return Ok(());
    }
    let target = novel.target_chapters;
    if target <= 0 {
        return Ok(());
    }
    let done_through = engine
        .db
        .list_chapters(novel_id)?
        .iter()
        .filter(|c| c.status == "done" && !c.content.trim().is_empty())
        .map(|c| c.idx)
        .max()
        .unwrap_or(0);
    if done_through >= target {
        engine.db.set_novel_auto_continue(novel_id, false)?;
        engine.hub.publish(novel_id, EngineEvent::Stage {
            stage: "done".into(),
            message: format!("自动连载完成：已达到目标 {target} 章"),
        });
        return Ok(());
    }
    // 预算护栏
    let settings = engine.db.pipeline_settings().unwrap_or_default();
    if settings.monthly_budget_tokens > 0 {
        let used = engine
            .db
            .usage_total_since(&crate::api::month_start_utc())
            .unwrap_or(0);
        if used >= settings.monthly_budget_tokens {
            engine.db.set_novel_auto_continue(novel_id, false)?;
            engine.hub.publish(novel_id, EngineEvent::Stage {
                stage: "budget".into(),
                message: format!(
                    "本月 Token 预算已达上限（{used}/{budget}），自动连载已暂停",
                    budget = settings.monthly_budget_tokens
                ),
            });
            return Ok(());
        }
    }
    let next = auto_batch_target(&engine.db, novel_id, target, settings.batch())?;
    tracing::info!("auto-continue: 启动下一批（至第 {next} 章）");
    engine.hub.publish(novel_id, EngineEvent::Stage {
        stage: "auto".into(),
        message: format!("自动连载：继续写下一批（至第 {next} 章，已完成 {done_through} 章）…"),
    });
    spawn(engine.clone(), novel_id, next)?;
    Ok(())
}

/// 创建一次运行并在后台执行；立即返回 run_id。
/// 断点续跑：重复调用同一 novel 时，已完成的章节会跳过。
pub fn spawn(engine: Arc<Engine>, novel_id: &str, chapters: i64) -> anyhow::Result<String> {    let run = engine.db.create_run(novel_id, chapters)?;
    let run_id = run.id.clone();
    engine.db.set_novel_status(novel_id, "writing")?;
    engine.hub.publish(novel_id, EngineEvent::RunStarted {
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
            engine2.hub.publish(&nid, EngineEvent::RunFailed { run_id: rid, error: msg });
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
    let settings = engine.llm_settings()?;
    let llm = Llm::new(settings.clone());
    let ov = engine.prompt_overrides();

    // ---- 1) 立项方案 ----
    let mut novel = engine
        .db
        .get_novel(novel_id)?
        .ok_or_else(|| anyhow::anyhow!("novel not found: {novel_id}"))?;
    if novel.title.trim().is_empty() {
        engine.hub.publish(novel_id, EngineEvent::Stage {
            stage: "brief".into(),
            message: "生成立项方案…".into(),
        });
        let (sys, usr) = crate::prompts::brief_prompt(
            &novel.idea,
            crate::styles::find(&novel.style_preset),
            &ov,
        );
        // JSON 解析失败自动重试一次（自愈）
        let mut raw = String::new();
        let mut parsed: Option<serde_json::Value> = None;
        for attempt in 1..=2u8 {
            let out = llm.complete(&sys, &usr).await?;
            record_usage(&engine.db, Some(novel_id), None, "brief", &settings, &out.usage);
            raw = out.text;
            match extract_json_object(&raw) {
                Some(v) => {
                    parsed = Some(v);
                    break;
                }
                None if attempt < 2 => {
                    engine.hub.publish(novel_id, EngineEvent::Stage {
                        stage: "brief".into(),
                        message: "立项输出不是 JSON，自动重试一次…".into(),
                    });
                }
                None => {}
            }
        }
        let v = parsed.ok_or_else(|| {
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
        engine.hub.publish(novel_id, EngineEvent::Stage {
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
        let (sys, usr) = crate::prompts::outline_prompt(
            &brief,
            from,
            to,
            prev_tail,
            crate::styles::find(&novel.style_preset),
            &ov,
        );
        // JSON 解析失败自动重试一次（自愈）
        let mut raw = String::new();
        let mut parsed: Option<serde_json::Value> = None;
        for attempt in 1..=2u8 {
            let out = llm.complete(&sys, &usr).await?;
            record_usage(&engine.db, Some(novel_id), None, "outline", &settings, &out.usage);
            raw = out.text;
            match extract_json_object(&raw) {
                Some(v) => {
                    parsed = Some(v);
                    break;
                }
                None if attempt < 2 => {
                    engine.hub.publish(novel_id, EngineEvent::Stage {
                        stage: "outline".into(),
                        message: "章节规划输出不是 JSON，自动重试一次…".into(),
                    });
                }
                None => {}
            }
        }
        let v = parsed.ok_or_else(|| {
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

        // 按阶段确认：本次做了规划 → 暂停等待人工检查 / 编辑章纲后再继续
        if novel.mode == "staged" {
            engine.db.update_run(run_id, "paused", "await_confirm", 0, None)?;
            engine.db.set_novel_status(novel_id, "await_confirm")?;
            engine.hub.publish(novel_id, EngineEvent::RunPaused {
                run_id: run_id.to_string(),
                novel_id: novel_id.to_string(),
                message: "章节规划已生成，确认后可继续写作".into(),
            });
            return Ok(());
        }
    }
    let novel = engine.db.get_novel(novel_id)?.unwrap_or(novel);

    // ---- 3) 逐章：写正文 + 审校 ----
    for idx in 1..=chapters {
        if engine.db.get_novel(novel_id)?.is_none() {
            // 作品已被删除：静默结束本次运行
            return Ok(());
        }
        // 预算护栏：超限则优雅收尾（已写章节保留；自动连载由收尾逻辑关闭）
        {
            let pipeline_settings = engine.db.pipeline_settings().unwrap_or_default();
            if pipeline_settings.monthly_budget_tokens > 0 {
                let used = engine
                    .db
                    .usage_total_since(&crate::api::month_start_utc())
                    .unwrap_or(0);
                if used >= pipeline_settings.monthly_budget_tokens {
                    engine.hub.publish(novel_id, EngineEvent::Stage {
                        stage: "budget".into(),
                        message: format!(
                            "本月 Token 预算已达上限（{used}/{budget}），本次运行提前收尾",
                            budget = pipeline_settings.monthly_budget_tokens
                        ),
                    });
                    break;
                }
            }
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

        engine.hub.publish(novel_id, EngineEvent::Stage {
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
        let ledger = ledger_context(&engine.db, novel_id, idx).unwrap_or_default();
        let recent = recent_summaries_context(&engine.db, novel_id, idx).unwrap_or_default();
        let context = format!("{recent}{ledger}");
        let (sys, usr) = crate::prompts::chapter_prompt(
            &brief,
            &novel.book_outline,
            &plan,
            &prev_tail,
            &context,
            crate::styles::find(&novel.style_preset),
            &ov,
        );
        // 流式输出按 ~80 字聚合后再推送；空内容自动重试一次（章节级自愈）
        let mut content = String::new();
        for attempt in 1..=2u8 {
            let mut buf = String::new();
            let hub = engine.hub.clone();
            let streamed = llm
                .stream(&sys, &usr, |delta: &str| {
                    buf.push_str(delta);
                    if buf.chars().count() >= 80 {
                        let chunk = std::mem::take(&mut buf);
                        hub.publish(
                            novel_id,
                            EngineEvent::Token {
                                chapter_idx: idx,
                                delta: chunk,
                            },
                        );
                    }
                })
                .await?;
            record_usage(
                &engine.db,
                Some(novel_id),
                Some(idx),
                "chapter",
                &settings,
                &streamed.usage,
            );
            if !buf.is_empty() {
                let chunk = std::mem::take(&mut buf);
                engine.hub.publish(novel_id, EngineEvent::Token {
                    chapter_idx: idx,
                    delta: chunk,
                });
            }
            let text = streamed.text.trim().to_string();
            if !text.is_empty() {
                content = text;
                break;
            }
            if attempt < 2 {
                engine.hub.publish(novel_id, EngineEvent::Stage {
                    stage: "draft".into(),
                    message: format!("第 {idx} 章返回空内容，自动重试一次…"),
                });
            } else {
                anyhow::bail!("第 {idx} 章生成内容为空（已自动重试）");
            }
        }

        // ---- 文风一致性检查（本地、零 token）----
        // 命中硬指标（段落等长 / 对话占比严重不足 / 禁用短语等）时自动重写一次，取表现更好的一稿。
        let style = crate::styles::find(&novel.style_preset);
        let mut content = content;
        let mut report = style.map(|p| crate::style_check::check_chapter(p, &content));
        if let (Some(p), Some(r)) = (style, report.as_ref()) {
            if r.hard_fail {
                engine.hub.publish(novel_id, EngineEvent::Stage {
                    stage: "draft".into(),
                    message: format!(
                        "第 {idx} 章未通过文风检查（{}），自动重写一次…",
                        r.summary()
                    ),
                });
                let usr_retry = format!(
                    "{usr}\n\n【上一稿未通过文风检查，请重写】问题：{}。请严格按文风预设修正后重写本章。",
                    r.summary()
                );
                let mut buf2 = String::new();
                let hub2 = engine.hub.clone();
                let streamed2 = llm
                    .stream(&sys, &usr_retry, |delta: &str| {
                        buf2.push_str(delta);
                        if buf2.chars().count() >= 80 {
                            let chunk = std::mem::take(&mut buf2);
                            hub2.publish(novel_id, EngineEvent::Token {
                                chapter_idx: idx,
                                delta: chunk,
                            });
                        }
                    })
                    .await?;
                record_usage(
                    &engine.db,
                    Some(novel_id),
                    Some(idx),
                    "chapter_retry",
                    &settings,
                    &streamed2.usage,
                );
                if !buf2.is_empty() {
                    let chunk = std::mem::take(&mut buf2);
                    engine.hub.publish(novel_id, EngineEvent::Token {
                        chapter_idx: idx,
                        delta: chunk,
                    });
                }
                let c2 = streamed2.text.trim().to_string();
                if !c2.is_empty() {
                    let r2 = crate::style_check::check_chapter(p, &c2);
                    if r2.score() >= r.score() {
                        content = c2;
                        report = Some(r2);
                    }
                }
            }
        }

        engine.db.save_chapter_content(novel_id, idx, &content, "drafted")?;
        if let Some(r) = &report {
            engine.db
                .save_chapter_style_report(novel_id, idx, &serde_json::to_string(r)?)?;
        }
        engine.hub.publish(novel_id, EngineEvent::ChapterSaved {
            chapter_idx: idx,
            title: ch.title.clone(),
            words: content.chars().count(),
        });

        // 审校 + 自动修复（公共函数：与「一键 AI 处理全部审校」共用同一逻辑）
        if let Err(e) = review_and_fix_chapter(
            &engine,
            novel_id,
            idx,
            &ch.title,
            &ch.goal,
            &mut content,
            style,
            &ov,
        )
        .await
        {
            tracing::warn!("chapter {idx} review failed: {e:#}");
        }
        // 写后资料整理：抽取角色 / 伏笔台账与章节摘要（非阻塞，失败仅记录日志）
        engine.hub.publish(novel_id, EngineEvent::Stage {
            stage: "extract".into(),
            message: format!("整理第 {idx} 章资料…"),
        });
        let (xsys, xusr) = crate::prompts::extract_prompt(idx, &content, &ov);
        match llm.complete(&xsys, &xusr).await {
            Ok(out) => {
                record_usage(&engine.db, Some(novel_id), Some(idx), "extract", &settings, &out.usage);
                let raw = out.text;
                if let Some(j) = extract_json_object(&raw) {
                    let (chars, fores) = apply_extraction(&engine.db, novel_id, idx, &j);
                    engine.hub.publish(novel_id, EngineEvent::LedgerUpdated {
                        chapter_idx: idx,
                        characters: chars,
                        foreshadowings: fores,
                    });
                }
            }
            Err(e) => tracing::warn!("chapter {idx} extract failed: {e:#}"),
        }

        engine.db.save_chapter_content(novel_id, idx, &content, "done")?;
        engine.db.update_run(run_id, "running", "draft", idx, None)?;
    }

    engine.db.update_run(run_id, "done", "done", chapters, None)?;
    engine.db.set_novel_status(novel_id, "drafted")?;
    engine.hub.publish(novel_id, EngineEvent::RunFinished {
        run_id: run_id.to_string(),
    });

    // 自动连载：达标/超预算/手动停止则在此收尾，否则启动下一批
    maybe_chain_auto(&engine, novel_id)?;
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

/// 统一的「写后资料整理」应用：角色 / 伏笔 / 章节摘要（流水线与单章接口共用）
pub fn apply_extraction(
    db: &crate::db::Db,
    novel_id: &str,
    chapter_idx: i64,
    j: &serde_json::Value,
) -> (usize, usize) {
    let chars = db.absorb_characters(novel_id, j).unwrap_or(0);
    let fores = db.absorb_foreshadowings(novel_id, chapter_idx, j).unwrap_or(0);
    if let Some(s) = j["summary"].as_str() {
        let s = s.trim();
        if !s.is_empty() {
            let _ = db.set_chapter_summary(novel_id, chapter_idx, s);
        }
    }
    (chars, fores)
}

/// 最近章节回顾：取当前章之前、带摘要的最近 5 章拼成上下文
fn recent_summaries_context(db: &crate::db::Db, novel_id: &str, current_idx: i64) -> anyhow::Result<String> {
    let chapters = db.list_chapters(novel_id)?;
    let mut lines: Vec<String> = chapters
        .iter()
        .filter(|c| c.idx < current_idx && !c.summary.trim().is_empty())
        .map(|c| format!("第{}章 {}：{}", c.idx, c.title, c.summary.trim()))
        .collect();
    if lines.is_empty() {
        return Ok(String::new());
    }
    let keep = lines.len().saturating_sub(5);
    lines.drain(0..keep);
    Ok(format!("最近章节回顾：\n{}\n", lines.join("\n")))
}

/// 组装台账上下文：活跃角色事实（最近 6 人 × 3 条）+ 未回收伏笔（最多 8 条，标注已拖延章数）
fn ledger_context(db: &crate::db::Db, novel_id: &str, current_idx: i64) -> anyhow::Result<String> {
    let mut s = String::new();
    let chars = db.list_characters(novel_id)?;
    let active: Vec<_> = chars.iter().filter(|c| c.status != "dead").collect();
    if !active.is_empty() {
        s.push_str("角色事实：\n");
        for c in active.iter().take(6) {
            let mut facts: Vec<&String> = c.facts.iter().rev().take(3).collect();
            facts.reverse();
            let facts = facts.iter().map(|f| f.as_str()).collect::<Vec<_>>().join("；");
            s.push_str(&format!("- {}（{}·{}）：{}\n", c.name, c.role, c.status, facts));
        }
    }
    let fores = db.list_foreshadowings(novel_id)?;
    let open: Vec<_> = fores.iter().filter(|f| f.status == "open").collect();
    if !open.is_empty() {
        s.push_str("未回收伏笔（若剧情合适，请在本章推进或回收）：\n");
        for f in open.iter().take(8) {
            let age = (current_idx - f.planted_chapter).max(0);
            let hint = if age >= 10 {
                format!("，已 {age} 章未回收，请优先安排回收")
            } else if age >= 5 {
                format!("，已 {age} 章未回收")
            } else {
                String::new()
            };
            s.push_str(&format!(
                "- {}（第{}章埋下{}）\n",
                f.title, f.planted_chapter, hint
            ));
        }
    }
    Ok(s)
}

/// 审校一章并按意见自动修复（流水线与「一键 AI 处理全部审校」共用）。
/// - 高/中危意见且开启 auto_fix 时：自动修订一次 → 复核 → 仅在问题变少时采用新稿
/// - 本地文风检查结果并入审校意见（内容变化时重算并回存）
/// 返回最终审校结果；`content_ref` 可能被替换为修复稿。
pub async fn review_and_fix_chapter(
    engine: &Arc<Engine>,
    novel_id: &str,
    idx: i64,
    title: &str,
    goal: &str,
    content_ref: &mut String,
    style: Option<&crate::styles::StylePreset>,
    ov: &crate::prompts::Overrides,
) -> anyhow::Result<Review> {
    let settings = engine.llm_settings()?;
    let llm = crate::llm::Llm::new(settings.clone());
    let mut content: String = content_ref.clone();
    let mut report = style.map(|p| crate::style_check::check_chapter(p, &content));
    let mut parsed: Option<Review> = None;

        // 审校：非阻塞 —— 失败只记录日志，不中断生产链
        engine.hub.publish(novel_id, EngineEvent::Stage {
            stage: "review".into(),
            message: format!("审校第 {idx} 章…"),
        });
        let (rsys, rusr) = crate::prompts::review_prompt(&content, style, &ov);
        match llm.complete(&rsys, &rusr).await {
            Ok(out) => {
                record_usage(&engine.db, Some(novel_id), Some(idx), "review", &settings, &out.usage);
                let raw = out.text;
                if let Some(j) = extract_json_object(&raw) {
                    let mut review = Review {
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

                    // ---- 自动修复（减少人工介入）----
                    // 审校出现高/中危问题时：按意见自动修订一次并复核，仅在问题变少时采用新稿。
                    let pipeline_settings = engine.db.pipeline_settings().unwrap_or_default();
                    let fixable: Vec<String> = review
                        .issues
                        .iter()
                        .filter(|i| i.severity == "high" || i.severity == "medium")
                        .map(|i| i.note.trim().to_string())
                        .filter(|n| !n.is_empty())
                        .take(6)
                        .collect();
                    if pipeline_settings.auto_fix_review && !fixable.is_empty() {
                        engine.hub.publish(novel_id, EngineEvent::Stage {
                            stage: "autofix".into(),
                            message: format!(
                                "按审校意见自动修复第 {idx} 章（{} 条问题）…",
                                fixable.len()
                            ),
                        });
                        let instruction = fixable.join("；");
                        let (fsys, fusr) = crate::prompts::rewrite_prompt(
                            "revise",
                            title,
                            goal,
                            &content,
                            &instruction,
                            style,
                            &ov,
                        );
                        match llm.complete(&fsys, &fusr).await {
                            Ok(fout) => {
                                record_usage(
                                    &engine.db,
                                    Some(novel_id),
                                    Some(idx),
                                    "auto_fix",
                                    &settings,
                                    &fout.usage,
                                );
                                let fixed = fout.text.trim().to_string();
                                if !fixed.is_empty() && fixed != content {
                                    // 复核修复稿：仅在问题变少时采用
                                    let (r2sys, r2usr) =
                                        crate::prompts::review_prompt(&fixed, style, &ov);
                                    let rechecked: Option<Vec<ReviewIssue>> =
                                        match llm.complete(&r2sys, &r2usr).await {
                                            Ok(r2out) => {
                                                record_usage(
                                                    &engine.db,
                                                    Some(novel_id),
                                                    Some(idx),
                                                    "review",
                                                    &settings,
                                                    &r2out.usage,
                                                );
                                                extract_json_object(&r2out.text).map(|j2| {
                                                    j2["issues"]
                                                        .as_array()
                                                        .map(|a| {
                                                            a.iter()
                                                                .map(|it| ReviewIssue {
                                                                    severity: str_field(
                                                                        it, "severity", "info",
                                                                    ),
                                                                    note: str_field(it, "note", ""),
                                                                    quote: str_field(
                                                                        it, "quote", "",
                                                                    ),
                                                                })
                                                                .collect()
                                                        })
                                                        .unwrap_or_default()
                                                })
                                            }
                                            Err(e) => {
                                                tracing::warn!(
                                                    "chapter {idx} re-review failed: {e:#}"
                                                );
                                                None
                                            }
                                        };
                                    let better = rechecked
                                        .as_ref()
                                        .map(|list| list.len() < review.issues.len())
                                        .unwrap_or(false);
                                    if better {
                                        let before = review.issues.len();
                                        content = fixed;
                                        review.issues = rechecked.unwrap_or_default();
                                        review.ok = review.issues.is_empty();
                                        // 内容已变化：重算本地文风报告并回存
                                        if let Some(p) = style {
                                            let r2 = crate::style_check::check_chapter(p, &content);
                                            let _ = engine.db.save_chapter_style_report(
                                                novel_id,
                                                idx,
                                                &serde_json::to_string(&r2)?,
                                            );
                                            report = Some(r2);
                                        }
                                        engine.db.save_chapter_content(
                                            novel_id, idx, &content, "drafted",
                                        )?;
                                        engine.hub.publish(novel_id, EngineEvent::ChapterSaved {
                                            chapter_idx: idx,
                                            title: title.to_string(),
                                            words: content.chars().count(),
                                        });
                                        engine.hub.publish(novel_id, EngineEvent::Stage {
                                            stage: "autofix".into(),
                                            message: format!(
                                                "第 {idx} 章自动修复完成（{before} → {} 条问题）",
                                                review.issues.len()
                                            ),
                                        });
                                    } else {
                                        engine.hub.publish(novel_id, EngineEvent::Stage {
                                            stage: "autofix".into(),
                                            message: format!(
                                                "第 {idx} 章自动修复未改善，保留原稿"
                                            ),
                                        });
                                    }
                                }
                            }
                            Err(e) => tracing::warn!("chapter {idx} auto-fix failed: {e:#}"),
                        }
                    }

                    // 本地文风检查结果并入审校意见，保持单一入口
                    if let Some(r) = &report {
                        review.issues.extend(r.as_review_issues());
                    }
                    engine.db.save_chapter_review(novel_id, idx, &review)?;
                    engine.hub.publish(novel_id, EngineEvent::ChapterReview {
                        chapter_idx: idx,
                        ok: review.ok,
                        issues: review.issues.len(),
                    });
                    parsed = Some(review);
                }
            }
            Err(e) => tracing::warn!("chapter {idx} review failed: {e:#}"),
        }

    let review = parsed.ok_or_else(|| anyhow::anyhow!("审校输出无法解析为 JSON"))?;
    *content_ref = content;
    Ok(review)
}

/// 章节是否还有待处理的审校（无审校记录，或存在高/中危意见）
fn review_needs_processing(ch: &crate::models::Chapter) -> bool {
    match &ch.review_json {
        None => true,
        Some(raw) => serde_json::from_str::<Review>(raw)
            .map(|r| medium_high_count(&r) > 0)
            .unwrap_or(true),
    }
}

fn medium_high_count(review: &Review) -> usize {
    review
        .issues
        .iter()
        .filter(|i| i.severity == "high" || i.severity == "medium")
        .count()
}

/// 一键「AI 处理全部审校」：对有内容的、存在待处理问题的章节重新审校并自动修复。
/// 返回待处理章节数（0 = 都已处理）；后台执行，进度经 SSE 推送。
pub fn spawn_review_all(engine: Arc<Engine>, novel_id: &str) -> anyhow::Result<i64> {
    let novel = engine
        .db
        .get_novel(novel_id)?
        .ok_or_else(|| anyhow::anyhow!("novel not found: {novel_id}"))?;
    let chapters = engine.db.list_chapters(novel_id)?;
    let targets: Vec<(i64, String, String)> = chapters
        .iter()
        .filter(|c| !c.content.trim().is_empty())
        .filter(|c| review_needs_processing(c))
        .map(|c| (c.idx, c.title.clone(), c.goal.clone()))
        .collect();
    if targets.is_empty() {
        return Ok(0);
    }
    let total = targets.len() as i64;
    let style_id = novel.style_preset.clone();
    let nid = novel_id.to_string();
    tokio::spawn(async move {
        let style = crate::styles::find(&style_id);
        let ov = engine.prompt_overrides();
        let mut digested = 0usize;
        for (i, (idx, title, goal)) in targets.iter().enumerate() {
            engine.hub.publish(&nid, EngineEvent::Stage {
                stage: "review".into(),
                message: format!("AI 处理审校：第 {idx} 章（{}/{total}）…", i + 1),
            });
            let Some(ch) = engine.db.get_chapter(&nid, *idx).ok().flatten() else {
                continue;
            };
            let before = ch
                .review_json
                .as_deref()
                .and_then(|raw| serde_json::from_str::<Review>(raw).ok())
                .map(|r| medium_high_count(&r))
                .unwrap_or(0);
            let mut content = ch.content.clone();
            match review_and_fix_chapter(&engine, &nid, *idx, title, goal, &mut content, style, &ov)
                .await
            {
                Ok(r) => {
                    digested += before.saturating_sub(medium_high_count(&r));
                }
                Err(e) => tracing::warn!("review-all chapter {idx} failed: {e:#}"),
            }
        }
        engine.hub.publish(&nid, EngineEvent::Stage {
            stage: "done".into(),
            message: format!("AI 审校处理完成：复核 {total} 章，消化 {digested} 条问题"),
        });
    });
    Ok(total)
}
