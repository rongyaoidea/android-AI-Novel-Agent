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

/// 带作品归属的广播事件（SSE 按作品过滤，避免串台）
#[derive(Clone)]
pub struct StampedEvent {
    pub novel_id: String,
    pub event: EngineEvent,
}

/// 事件中心：广播给 SSE 订阅者 + 每书保留最近 N 条关键事件的环形缓冲
/// （刷新页面后重连可回放到日志面板；Token 流不进入缓冲）
#[derive(Clone)]
pub struct EventHub {
    tx: broadcast::Sender<StampedEvent>,
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
        let _ = self.tx.send(StampedEvent {
            novel_id: novel_id.to_string(),
            event: ev,
        });
    }

    /// 某本书的关键事件历史（用于 SSE 重连回放）
    pub fn history(&self, novel_id: &str) -> Vec<EngineEvent> {
        self.history
            .lock()
            .ok()
            .and_then(|h| h.get(novel_id).map(|q| q.iter().cloned().collect()))
            .unwrap_or_default()
    }

    pub fn subscribe(&self) -> broadcast::Receiver<StampedEvent> {
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

/// 已写完的最后一章章号（状态 done 且正文非空）
fn done_through(db: &crate::db::Db, novel_id: &str) -> anyhow::Result<i64> {
    Ok(db
        .list_chapters(novel_id)?
        .iter()
        .filter(|c| c.status == "done" && !c.content.trim().is_empty())
        .map(|c| c.idx)
        .max()
        .unwrap_or(0))
}

/// 自动连载的下一批目标截止章号 = min(已写完章号 + 每批章数, 全书目标)。
/// 以「已写完」为基准（而非已规划），避免滚动规划产生的超前计划让一次写过多章。
pub fn auto_batch_target(
    db: &crate::db::Db,
    novel_id: &str,
    target: i64,
    batch: i64,
) -> anyhow::Result<i64> {
    let done = done_through(db, novel_id)?;
    Ok((done + batch.clamp(1, 50)).min(target.max(0)))
}

/// 「继续创作」的默认终点：从已写完处再写一批（受全书目标约束；未设目标时按批大小前进）
pub fn run_batch_end(db: &crate::db::Db, novel_id: &str, batch: i64) -> anyhow::Result<i64> {
    let novel = db
        .get_novel(novel_id)?
        .ok_or_else(|| anyhow::anyhow!("novel not found: {novel_id}"))?;
    let done = done_through(db, novel_id)?;
    let batch = batch.clamp(1, 50);
    let target = if novel.target_chapters > 0 {
        novel.target_chapters
    } else {
        done + batch
    };
    Ok((done + batch).min(target))
}

/// 规划并落库 [from, to] 区间的章纲（一次 LLM 调用；已开写/已定稿章节不会被覆盖）。
/// 滚动规划的公共入口：run 开头规划首个窗口，写到窗口尾部前再续规划下一段。
#[allow(clippy::too_many_arguments)]
async fn plan_range(
    engine: &Arc<Engine>,
    novel: &Novel,
    novel_id: &str,
    from: i64,
    to: i64,
    llm: &Llm,
    ov: &crate::prompts::Overrides,
    settings: &LlmSettings,
) -> anyhow::Result<()> {
    if to < from {
        return Ok(());
    }
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
        worldview: novel.worldview.clone(),
    };
    let existing = engine.db.list_chapters(novel_id)?;
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
        ov,
    );
    // JSON 解析失败自动重试一次（自愈）
    let mut raw = String::new();
    let mut parsed: Option<serde_json::Value> = None;
    for attempt in 1..=2u8 {
        let out = llm.complete(&sys, &usr).await?;
        record_usage(&engine.db, Some(novel_id), None, "outline", settings, &out.usage);
        raw = out.text;
        match extract_json_object(&raw) {
            Some(v) => {
                parsed = Some(v);
                break;
            }
            None if attempt < 2 => {
                engine.hub.publish(novel_id, EngineEvent::Stage {
                    stage: "outline".into(),
                    message: format!("第 {from}~{to} 章规划输出不是 JSON，自动重试一次…"),
                });
            }
            None => {}
        }
    }
    let v = parsed.ok_or_else(|| {
        anyhow::anyhow!(
            "outline 输出不是 JSON：{}",
            raw.chars().take(200).collect::<String>()
        )
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
            // 节奏字段（章末钩型 / 推进强度）：模型未输出时保持旧值
            if item.get("hook_type").is_some() || item.get("intensity").is_some() {
                let hook_type = item["hook_type"].as_str().unwrap_or("").trim().to_string();
                let intensity = item["intensity"].as_i64().unwrap_or(0).clamp(0, 5);
                engine
                    .db
                    .set_chapter_pacing(novel_id, idx, &hook_type, intensity)?;
            }
        }
    }
    Ok(())
}

/// 运行成功收尾：若开启自动连载且未达目标且未超预算 → 自动启动下一批。
/// 停止条件：目标达成（自动关闭）/ 预算耗尽（自动关闭并提示）/ 手动关闭。
fn maybe_chain_auto(engine: &Arc<Engine>, novel_id: &str) -> anyhow::Result<()> {
    let Some(novel) = engine.db.get_novel(novel_id)? else {
        return Ok(());
    };
    // 手动停止：不启动下一批（并清掉标记，避免影响之后的手动「继续创作」）
    if engine.stop_requested(novel_id) {
        engine.clear_stop(novel_id);
        engine.db.set_novel_auto_continue(novel_id, false)?;
        return Ok(());
    }
    // 审校处理进行中：本轮不接下一批（避免与审校任务并发互写正文）
    if engine.is_reviewing(novel_id) {
        engine.hub.publish(novel_id, EngineEvent::Stage {
            stage: "auto".into(),
            message: "审校处理进行中，本批暂不自动续写；处理完成后可再点「自动连载」".into(),
        });
        return Ok(());
    }
    if !novel.auto_continue {
        return Ok(());
    }
    let target = novel.target_chapters;
    if target <= 0 {
        return Ok(());
    }
    let done_through = done_through(&engine.db, novel_id)?;
    if done_through >= target {
        engine.db.set_novel_auto_continue(novel_id, false)?;
        engine.hub.publish(novel_id, EngineEvent::Stage {
            stage: "done".into(),
            message: format!("自动连载完成：已达到目标 {target} 章"),
        });
        // 完本收尾：读者审查只在完本时自动执行（全局审校 + 卷末体检补齐 + 读者审查报告）
        if let Err(e) = spawn_finish_book(engine.clone(), novel_id, false) {
            tracing::warn!("finish book spawn failed: {e:#}");
        }
        return Ok(());
    }
    // 预算护栏：超限不关开关，转入「待机」，下个自然月自动恢复
    let settings = engine.db.pipeline_settings().unwrap_or_default();
    if settings.monthly_budget_tokens > 0 {
        let used = engine
            .db
            .usage_total_since(&crate::api::month_start_local())
            .unwrap_or(0);
        if used >= settings.monthly_budget_tokens {
            engine.mark_budget_wait(novel_id);
            engine.hub.publish(novel_id, EngineEvent::Stage {
                stage: "budget".into(),
                message: format!(
                    "本月 Token 预算已达上限（{used}/{budget}）：转入待机，下个自然月自动恢复连载（也可调大预算后手动继续）",
                    budget = settings.monthly_budget_tokens
                ),
            });
            schedule_budget_resume(engine, novel_id);
            return Ok(());
        }
    }
    engine.clear_budget_wait(novel_id);
    let next = auto_batch_target(&engine.db, novel_id, target, settings.batch())?;
    tracing::info!("auto-continue: 启动下一批（至第 {next} 章）");
    engine.hub.publish(novel_id, EngineEvent::Stage {
        stage: "auto".into(),
        message: format!("自动连载：继续写下一批（至第 {next} 章，已完成 {done_through} 章）…"),
    });
    spawn(engine.clone(), novel_id, next)?;
    Ok(())
}

/// 失败重试的退避表（秒）：1 / 5 / 15 分钟，最多 3 次
const RETRY_DELAYS_SECS: [u64; 3] = [60, 300, 900];

/// 第 attempt 次重试的等待秒数（None = 已用尽）
fn retry_delay_secs(attempt: u32) -> Option<u64> {
    if attempt == 0 {
        return None;
    }
    RETRY_DELAYS_SECS.get((attempt - 1) as usize).copied()
}

/// 运行失败后的自动重试（仅自动连载体）：退避重跑该批；用尽后关闭自动连载并提示。
/// 重试前会复查开关/停止/达标/运行中/审校中/预算待机等条件。
fn maybe_retry_after_failure(engine: &Arc<Engine>, novel_id: &str) {
    let Ok(Some(novel)) = engine.db.get_novel(novel_id) else {
        return;
    };
    if !novel.auto_continue {
        return;
    }
    if !engine.mark_retry_scheduled(novel_id) {
        return; // 已有排期
    }
    let attempt = engine.bump_retry_attempt(novel_id);
    let Some(delay) = retry_delay_secs(attempt) else {
        engine.clear_retry(novel_id);
        let _ = engine.db.set_novel_auto_continue(novel_id, false);
        engine.hub.publish(novel_id, EngineEvent::Stage {
            stage: "auto".into(),
            message: "自动连载：连续 3 次失败，已停止自动重试并关闭开关（修好模型/网络后点「自动连载」可继续）".into(),
        });
        return;
    };
    engine.hub.publish(novel_id, EngineEvent::Stage {
        stage: "auto".into(),
        message: format!(
            "自动连载：本批失败，{} 分钟后自动重试（第 {attempt}/3 次，断点续写）",
            delay / 60
        ),
    });
    let engine2 = engine.clone();
    let nid = novel_id.to_string();
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_secs(delay)).await;
        engine2.unmark_retry_scheduled(&nid);
        // 重试前复查：开关仍在、未停止、未达标、没有进行中的运行/审校、不在预算待机
        let Ok(Some(novel)) = engine2.db.get_novel(&nid) else {
            return;
        };
        if !novel.auto_continue || engine2.stop_requested(&nid) || engine2.is_budget_waiting(&nid) {
            return;
        }
        if novel.target_chapters > 0 {
            let done = done_through(&engine2.db, &nid).unwrap_or(0);
            if done >= novel.target_chapters {
                return;
            }
        }
        if let Ok(Some(run)) = engine2.db.latest_run(&nid) {
            if run.status == "running" {
                return;
            }
        }
        if engine2.is_reviewing(&nid) {
            return; // 审校结束后由审校任务续批（G3）
        }
        let batch = engine2
            .db
            .pipeline_settings()
            .map(|s| s.batch())
            .unwrap_or(10);
        match run_batch_end(&engine2.db, &nid, batch) {
            Ok(end) => {
                if let Err(e) = spawn(engine2.clone(), &nid, end) {
                    tracing::warn!("auto retry spawn failed: {e:#}");
                }
            }
            Err(e) => tracing::warn!("auto retry compute failed: {e:#}"),
        }
    });
}

/// 预算待机：睡到下个自然月自动恢复（期间每 6 小时复查一次；应用重启后待机态丢失，需手动点一次继续）
fn schedule_budget_resume(engine: &Arc<Engine>, novel_id: &str) {
    if !engine.mark_budget_resume_scheduled(novel_id) {
        return;
    }
    let engine2 = engine.clone();
    let nid = novel_id.to_string();
    tokio::spawn(async move {
        loop {
            let now = chrono::Local::now();
            let until = (next_month_start_local(now) - now)
                .to_std()
                .unwrap_or(std::time::Duration::from_secs(3600));
            let sleep_for = until.min(std::time::Duration::from_secs(6 * 3600));
            if !sleep_for.is_zero() {
                tokio::time::sleep(sleep_for).await;
            }
            let Ok(Some(novel)) = engine2.db.get_novel(&nid) else {
                engine2.unmark_budget_resume_scheduled(&nid);
                engine2.clear_budget_wait(&nid);
                return;
            };
            if !novel.auto_continue {
                engine2.unmark_budget_resume_scheduled(&nid);
                engine2.clear_budget_wait(&nid);
                return;
            }
            let budget = engine2
                .db
                .pipeline_settings()
                .unwrap_or_default()
                .monthly_budget_tokens;
            let used = engine2
                .db
                .usage_total_since(&crate::api::month_start_local())
                .unwrap_or(0);
            if budget > 0 && used >= budget {
                continue; // 还没跨月或仍未恢复：继续等
            }
            engine2.unmark_budget_resume_scheduled(&nid);
            engine2.clear_budget_wait(&nid);
            engine2.hub.publish(&nid, EngineEvent::Stage {
                stage: "auto".into(),
                message: "预算待机结束：自动恢复连载".into(),
            });
            let _ = maybe_chain_auto(&engine2, &nid);
            return;
        }
    });
}

/// 下个自然月起点（本地时区 00:05，留 5 分钟缓冲）
fn next_month_start_local(now: chrono::DateTime<chrono::Local>) -> chrono::DateTime<chrono::Local> {
    use chrono::{Datelike, TimeZone};
    let (y, m) = (now.year(), now.month());
    let (ny, nm) = if m >= 12 { (y + 1, 1) } else { (y, m + 1) };
    chrono::Local
        .with_ymd_and_hms(ny, nm, 1, 0, 5, 0)
        .single()
        .unwrap_or(now)
}

/// 等待「手动停止」信号（250ms 轮询；与流式生成共享 select，实现中途中断）
async fn wait_for_stop(engine: &Arc<Engine>, novel_id: &str) {
    loop {
        if engine.stop_requested(novel_id) {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
    }
}

/// 手动停止的统一收尾：清标记 + 运行置 paused（stage=stopped）+ 推 RunPaused 事件
fn pause_for_stop(
    engine: &Arc<Engine>,
    novel_id: &str,
    run_id: &str,
    cursor: i64,
    message: Option<String>,
) -> anyhow::Result<()> {
    engine.clear_stop(novel_id);
    engine
        .db
        .update_run(run_id, "paused", "stopped", cursor.max(0), None)?;
    engine.hub.publish(novel_id, EngineEvent::RunPaused {
        run_id: run_id.to_string(),
        novel_id: novel_id.to_string(),
        message: message.unwrap_or_else(|| {
            "已按你的请求停止创作；已写章节全部保留，点「继续创作」可随时续写".to_string()
        }),
    });
    Ok(())
}

/// 创建一次运行并在后台执行；立即返回 run_id。
/// 断点续跑：重复调用同一 novel 时，已完成的章节会跳过。
pub fn spawn(engine: Arc<Engine>, novel_id: &str, chapters: i64) -> anyhow::Result<String> {    let run = engine.db.create_run(novel_id, chapters)?;
    let run_id = run.id.clone();
    // 手动停止标记在这里同步清除（任务启动前）：避免刚创建就被上一轮的残留标记误停，
    // 同时保证「创建之后到达的停止请求」一定生效（不会被任务启动时清掉）。
    engine.clear_stop(novel_id);
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
            // 无人值守：自动连载体失败后退避重试（用尽则关闭并提示）
            maybe_retry_after_failure(&engine2, &nid);
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

    // 开工巡检（轻量）：状态修正 + 缺摘要补齐 + 漏审复审；健康的书零开销（全部 bounded）
    match maintain_novel(&engine, novel_id, 3, 1).await {
        Ok(r) if r.touched() > 0 => {
            engine.hub.publish(novel_id, EngineEvent::Stage {
                stage: "extract".into(),
                message: format!(
                    "开工巡检：修正 {} 处状态，补 {} 条摘要，重审 {} 章{}",
                    r.fixed_empty_status,
                    r.filled_summaries,
                    r.revisited,
                    if r.stale_left > 0 {
                        format!("（仍余 {} 章待重审，可点「AI 处理全部审校」）", r.stale_left)
                    } else {
                        String::new()
                    }
                ),
            });
        }
        Ok(_) => {}
        Err(e) => tracing::warn!("pre-run maintenance failed: {e:#}"),
    }

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
            &parse_clarify(&novel.clarify_json),
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
            worldview: str_field(&v, "worldview", ""),
        };
        engine.db.update_novel_brief(novel_id, &brief)?;
        novel = engine.db.get_novel(novel_id)?.unwrap_or(novel);
        engine.db.update_run(run_id, "running", "outline", 0, None)?;
    }

    // ---- 2) 章节规划（滚动窗口：单次只规划前方 ≤plan_window 章，写到窗口尾部前自动续规划）----
    let plan_window = engine.db.pipeline_settings().unwrap_or_default().plan_window();
    let existing = engine.db.list_chapters(novel_id)?;
    let have: std::collections::BTreeSet<i64> = existing.iter().map(|c| c.idx).collect();
    let missing: Vec<i64> = (1..=chapters).filter(|i| !have.contains(i)).collect();
    let keep_ahead = (plan_window / 3).max(2);
    let mut planned_upto = chapters; // 无缺失时视为已规划完毕
    let mut next_plan_from: Option<i64> = None;
    if let Some(&first) = missing.first() {
        let to = (first + plan_window - 1).min(chapters);
        plan_range(&engine, &novel, novel_id, first, to, &llm, &ov, &settings).await?;
        planned_upto = to;
        if to < chapters {
            next_plan_from = Some(to + 1);
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
    let mut wrote_in_run = 0usize;
    for idx in 1..=chapters {
        if engine.db.get_novel(novel_id)?.is_none() {
            // 作品已被删除：静默结束本次运行
            return Ok(());
        }
        // 手动停止：在章与章之间退出（已写章节全部保留）
        if engine.stop_requested(novel_id) {
            pause_for_stop(&engine, novel_id, run_id, idx - 1, None)?;
            return Ok(());
        }
        // 预算护栏：超限则优雅收尾（已写章节保留；自动连载由收尾逻辑关闭）
        {
            let pipeline_settings = engine.db.pipeline_settings().unwrap_or_default();
            if pipeline_settings.monthly_budget_tokens > 0 {
                let used = engine
                    .db
                    .usage_total_since(&crate::api::month_start_local())
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

        // 滚动规划：接近当前规划窗口尾部时续规划下一段（吸收已写章节的实际走向，避免远期漂移）
        if next_plan_from.is_some() && idx > planned_upto - keep_ahead {
            let from = next_plan_from.take().unwrap_or(idx);
            let to = (from + plan_window - 1).min(chapters);
            plan_range(&engine, &novel, novel_id, from, to, &llm, &ov, &settings).await?;
            planned_upto = to;
            if to < chapters {
                next_plan_from = Some(to + 1);
            }
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
            worldview: novel.worldview.clone(),
        };
        let plan = ChapterPlan {
            idx: ch.idx,
            title: ch.title.clone(),
            goal: ch.goal.clone(),
            beats: ch.beats.clone(),
        };
        let ledger = ledger_context(&engine.db, novel_id, idx).unwrap_or_default();
        let recent = recent_summaries_context(&engine.db, novel_id, idx).unwrap_or_default();
        // 写前事实预检（本地规则，零 token）：矛盾点在生成前提醒模型
        let warnings = preflight_warnings(&engine.db, novel_id, idx, &plan, &ch.hook_type, ch.intensity);
        let preflight = if warnings.is_empty() {
            String::new()
        } else {
            engine.hub.publish(novel_id, EngineEvent::Stage {
                stage: "draft".into(),
                message: format!("写前预检：{} 条提醒已注入提示词", warnings.len()),
            });
            format!("【写前预检提醒（务必避免以下问题）】\n- {}\n", warnings.join("\n- "))
        };
        let context = format!("{preflight}{recent}{ledger}");
        // 节奏要求（章纲钩型 / 推进强度 → 提示词；未标注时为空）
        let mut hook_line = String::new();
        if !ch.hook_type.trim().is_empty() || ch.intensity > 0 {
            hook_line.push_str("【本章节奏要求】");
            if !ch.hook_type.trim().is_empty() {
                hook_line.push_str(&format!("章末钩型：{}；", ch.hook_type.trim()));
            }
            if ch.intensity > 0 {
                hook_line.push_str(&format!(
                    "推进强度 {}/5（1=过渡、3=小高潮、5=卷末爆点；强度低也要有实质推进，不能水字数）；",
                    ch.intensity
                ));
            }
            hook_line.push('\n');
        }
        let (sys, usr) = crate::prompts::chapter_prompt(
            &brief,
            &novel.book_outline,
            &plan,
            &prev_tail,
            &context,
            &hook_line,
            crate::styles::find(&novel.style_preset),
            &ov,
        );
        // 流式输出按 ~80 字聚合后再推送；空内容自动重试一次（章节级自愈）
        // 与「停止」信号共享 select：手动停止时立即中断流式生成
        let mut content = String::new();
        for attempt in 1..=2u8 {
            let mut buf = String::new();
            let hub = engine.hub.clone();
            let streamed_result = tokio::select! {
                r = llm.stream(&sys, &usr, |delta: &str| {
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
                }) => r,
                _ = wait_for_stop(&engine, novel_id) => {
                    pause_for_stop(
                        &engine,
                        novel_id,
                        run_id,
                        idx - 1,
                        Some(format!(
                            "已停止：第 {idx} 章写到一半被手动中止（本章草稿未保存），已写章节保留"
                        )),
                    )?;
                    return Ok(());
                }
            };
            // 空响应（内容过滤/兼容实现返回非 SSE 等）走「自动重试一次」，而不是让整批运行失败
            let streamed = match streamed_result {
                Ok(s) => s,
                Err(e) if e.downcast_ref::<crate::llm::EmptyStreamError>().is_some() => {
                    if attempt < 2 {
                        engine.hub.publish(novel_id, EngineEvent::Stage {
                            stage: "draft".into(),
                            message: format!("第 {idx} 章返回空内容，自动重试一次…"),
                        });
                        continue;
                    }
                    anyhow::bail!("第 {idx} 章生成内容为空（已自动重试）");
                }
                Err(e) => return Err(e),
            };
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
                let streamed2_result = tokio::select! {
                    r = llm.stream(&sys, &usr_retry, |delta: &str| {
                        buf2.push_str(delta);
                        if buf2.chars().count() >= 80 {
                            let chunk = std::mem::take(&mut buf2);
                            hub2.publish(novel_id, EngineEvent::Token {
                                chapter_idx: idx,
                                delta: chunk,
                            });
                        }
                    }) => r,
                    _ = wait_for_stop(&engine, novel_id) => {
                        pause_for_stop(
                            &engine,
                            novel_id,
                            run_id,
                            idx - 1,
                            Some(format!(
                                "已停止：第 {idx} 章重写中被手动中止，已写章节保留"
                            )),
                        )?;
                        return Ok(());
                    }
                };
                // 重写失败不致命：保留原稿继续后续流程
                match streamed2_result {
                    Ok(streamed2) => {
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
                    Err(e) => {
                        tracing::warn!("chapter {idx} style retry failed: {e:#}（保留原稿）");
                    }
                }
            }
        }

        engine.db.save_chapter_content_sourced(novel_id, idx, &content, "drafted", "draft")?;
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

        // 卷级摘要 + 卷末体检：每写满 VOLUME_SPAN 章（失败不阻断）
        if idx % VOLUME_SPAN == 0 {
            let volume_idx = idx / VOLUME_SPAN;
            match generate_volume_summary(&engine, novel_id, volume_idx).await {
                Ok(()) => {
                    if let Err(e) = generate_volume_review(&engine, novel_id, volume_idx).await {
                        tracing::warn!("volume review at {idx} failed: {e:#}");
                    }
                }
                Err(e) => tracing::warn!("volume summary at {idx} failed: {e:#}"),
            }
        }

        engine.db.save_chapter_content_sourced(novel_id, idx, &content, "done", "draft")?;
        engine.db.update_run(run_id, "running", "draft", idx, None)?;
        wrote_in_run += 1;
    }

    // 运行收尾：真实进度游标（预算/停止提前收尾时不虚报）+ 补写缺失卷摘要 + 伏笔自动审核
    let last_done = engine
        .db
        .list_chapter_metas(novel_id)
        .map(|cs| {
            cs.iter()
                .filter(|c| c.status == "done")
                .map(|c| c.idx)
                .max()
                .unwrap_or(0)
        })
        .unwrap_or(0);
    let _ = maybe_fill_volume_summaries(&engine, novel_id, last_done).await;
    // 伏笔自动审核（按设置间隔；1 = 每批都跑，N = 已完成章号逢 N 的倍数，0 = 关闭）
    if wrote_in_run > 0 {
        let settings = engine.db.pipeline_settings().unwrap_or_default();
        let every = settings.audit_every();
        if every > 0 && (every == 1 || last_done % every == 0) {
            let _ = audit_foreshadowings(&engine, novel_id).await;
        }
        // 全局审校（按间隔；覆盖跨章一致性/节奏/支线状态，含自动定向修复）
        let gevery = settings.global_every();
        if gevery > 0 && last_done % gevery == 0 {
            let _ = global_review_and_fix(&engine, novel_id, 2).await;
        }
    }

    engine.db.update_run(run_id, "done", "done", last_done, None)?;
    engine.db.set_novel_status(novel_id, "drafted")?;
    engine.hub.publish(novel_id, EngineEvent::RunFinished {
        run_id: run_id.to_string(),
    });

    // 自动连载：达标/超预算/手动停止则在此收尾，否则启动下一批（成功一轮 → 失败重试计数清零）
    engine.clear_retry(novel_id);
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

/// 卷大小：每写满 VOLUME_SPAN 章归并一条卷级摘要（长书回顾）
const VOLUME_SPAN: i64 = 20;

/// 前情回顾：已完结卷的卷级摘要（最近 3 卷）+ 当前章之前带摘要的最近 5 章
fn recent_summaries_context(db: &crate::db::Db, novel_id: &str, current_idx: i64) -> anyhow::Result<String> {
    let mut s = String::new();
    let volumes = db.list_volume_summaries(novel_id)?;
    let mut vols: Vec<&crate::models::VolumeSummary> = volumes
        .iter()
        .filter(|v| v.upto_chapter < current_idx && !v.summary.trim().is_empty())
        .collect();
    if vols.len() > 3 {
        vols.drain(0..vols.len() - 3);
    }
    if !vols.is_empty() {
        s.push_str("前情回顾（卷摘要）：\n");
        for v in vols {
            s.push_str(&format!(
                "- 第{}卷（至第{}章）：{}\n",
                v.volume_idx,
                v.upto_chapter,
                v.summary.trim()
            ));
        }
    }
    let chapters = db.list_chapter_summaries(novel_id)?;
    let mut lines: Vec<String> = chapters
        .iter()
        .filter(|(idx, _, summary)| *idx < current_idx && !summary.trim().is_empty())
        .map(|(idx, title, summary)| format!("第{}章 {}：{}", idx, title, summary.trim()))
        .collect();
    if !lines.is_empty() {
        let keep = lines.len().saturating_sub(5);
        lines.drain(0..keep);
        s.push_str(&format!("最近章节回顾：\n{}\n", lines.join("\n")));
    }
    Ok(s)
}

/// 写前事实预检（本地规则，零 token）：把与台账/近章/节奏矛盾的章纲点出来，随提示词注入。
/// 规则：① 章纲提到已死亡/退场角色（可能是回忆，但值得核对）；
///       ② 与最近 5 章目标高度重复（字符二元组 Jaccard ≥ 0.6，提示可能重复桥段）；
///       ③ 节奏预检（基于章纲钩型/强度字段）：连续过渡章、连续高强度、强钩同型连发。
fn preflight_warnings(
    db: &crate::db::Db,
    novel_id: &str,
    idx: i64,
    plan: &ChapterPlan,
    hook_type: &str,
    intensity: i64,
) -> Vec<String> {
    let mut warnings = Vec::new();
    let text = format!("{} {}", plan.goal, plan.beats.join(" "));
    if let Ok(chars) = db.list_characters(novel_id) {
        for c in chars.iter().filter(|c| c.status == "dead" || c.status == "leave") {
            let name = c.name.trim();
            if !name.is_empty() && text.contains(name) {
                warnings.push(format!(
                    "章纲提到已{}的角色「{}」：请确认是回忆/闪回，或调整章纲",
                    if c.status == "dead" { "死亡" } else { "退场" },
                    name
                ));
            }
        }
    }
    let metas = db.list_chapter_metas(novel_id).unwrap_or_default();
    for p in metas
        .iter()
        .filter(|c| c.idx < idx && c.idx > idx - 6 && !c.goal.trim().is_empty())
    {
        let sim = goal_similarity(plan.goal.trim(), p.goal.trim());
        if sim >= 0.6 {
            warnings.push(format!(
                "本章目标与第{}章目标高度相似（{:.0}%）：注意避免重复桥段",
                p.idx,
                sim * 100.0
            ));
        }
    }
    // ③ 节奏预检（强度 0 = 未标注，跳过）
    let cur_hook = hook_type.trim();
    if intensity > 0 {
        let prev1 = metas.iter().filter(|c| c.idx == idx - 1).max_by_key(|c| c.idx);
        let prev2 = metas.iter().filter(|c| c.idx == idx - 2).max_by_key(|c| c.idx);
        if let (Some(p1), Some(p2)) = (prev1, prev2) {
            if p1.intensity > 0
                && p2.intensity > 0
                && p1.intensity <= 2
                && p2.intensity <= 2
                && intensity <= 2
            {
                warnings.push(format!(
                    "节奏偏平：第{}、{}章与本章强度都 ≤2（连续过渡章）——本章需要明确推进/冲突/爆点，或调高本章强度",
                    p2.idx, p1.idx
                ));
            }
            if p1.intensity >= 4 && p2.intensity >= 4 && intensity >= 4 {
                warnings.push(
                    "连续高能：近三章强度都 ≥4，读者容易麻木——本章建议放缓或改用信息型推进"
                        .to_string(),
                );
            }
        }
        if let Some(p1) = prev1 {
            if !cur_hook.is_empty()
                && intensity >= 4
                && p1.intensity >= 4
                && p1.hook_type.trim() == cur_hook
            {
                warnings.push(format!(
                    "钩型重复：与上一章同为高强度「{cur_hook}」钩——建议更换钩型（悬念/危机/反转/升级/信息）"
                ));
            }
        }
    }
    warnings
}

/// 目标相似度：字符二元组 Jaccard（短文本也适用）
fn goal_similarity(a: &str, b: &str) -> f64 {
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }
    let bigrams = |s: &str| -> std::collections::HashSet<(char, char)> {
        let v: Vec<char> = s.chars().collect();
        v.windows(2).map(|w| (w[0], w[1])).collect()
    };
    let (ba, bb) = (bigrams(a), bigrams(b));
    if ba.is_empty() || bb.is_empty() {
        return 0.0;
    }
    let inter = ba.intersection(&bb).count() as f64;
    let union = ba.union(&bb).count() as f64;
    if union == 0.0 {
        0.0
    } else {
        inter / union
    }
}

/// 归并一卷（volume_idx 从 1 起，覆盖 (v-1)*SPAN+1 ..= v*SPAN）为一条卷级摘要。
/// 摘要为空的任务静默跳过；失败由调用方记录。
async fn generate_volume_summary(
    engine: &Arc<Engine>,
    novel_id: &str,
    volume_idx: i64,
) -> anyhow::Result<()> {
    let from = (volume_idx - 1) * VOLUME_SPAN + 1;
    let to = volume_idx * VOLUME_SPAN;
    let chapters = engine.db.list_chapter_summaries(novel_id)?;
    let items: Vec<(i64, String, String)> = chapters
        .iter()
        .filter(|(idx, _, summary)| *idx >= from && *idx <= to && !summary.trim().is_empty())
        .map(|(idx, title, summary)| (*idx, title.clone(), summary.trim().to_string()))
        .collect();
    if items.is_empty() {
        return Ok(());
    }
    engine.hub.publish(novel_id, EngineEvent::Stage {
        stage: "extract".into(),
        message: format!("归并第 {from}~{to} 章卷级摘要…"),
    });
    let settings = engine.llm_settings()?;
    let llm = Llm::new(settings.clone());
    let ov = engine.prompt_overrides();
    let (sys, usr) = crate::prompts::volume_summary_prompt(from, to, &items, &ov);
    let out = llm.complete(&sys, &usr).await?;
    record_usage(
        &engine.db,
        Some(novel_id),
        None,
        "volume_summary",
        &settings,
        &out.usage,
    );
    let raw = out.text;
    let summary = extract_json_object(&raw)
        .and_then(|v| v["summary"].as_str().map(|s| s.trim().to_string()))
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| raw.trim().chars().take(300).collect());
    if summary.trim().is_empty() {
        return Ok(());
    }
    engine
        .db
        .upsert_volume_summary(novel_id, volume_idx, to, summary.trim())?;
    engine.hub.publish(novel_id, EngineEvent::Stage {
        stage: "extract".into(),
        message: format!("第 {volume_idx} 卷摘要已生成（第 {from}~{to} 章）"),
    });
    Ok(())
}

/// 卷末体检（单次调用；去重与「修复→复检」闭环在 generate_volume_review 包装层）。
/// 卷摘要 + 逐章概览（强度/钩型/字数/摘要）+ 未回收伏笔 + 本地文体统计 → 报告存 volume_reports。
async fn volume_review_once(
    engine: &Arc<Engine>,
    novel_id: &str,
    volume_idx: i64,
) -> anyhow::Result<()> {
    let from = (volume_idx - 1) * VOLUME_SPAN + 1;
    let to = volume_idx * VOLUME_SPAN;
    let metas = engine.db.list_chapter_metas(novel_id)?;
    let summaries: std::collections::HashMap<i64, String> = engine
        .db
        .list_chapter_summaries(novel_id)?
        .into_iter()
        .map(|(idx, _, s)| (idx, s))
        .collect();
    let mut lines: Vec<String> = Vec::new();
    let mut contents: Vec<(i64, String)> = Vec::new();
    for m in metas.iter().filter(|m| m.idx >= from && m.idx <= to) {
        let sum: String = summaries
            .get(&m.idx)
            .map(|s| s.trim().chars().take(50).collect())
            .unwrap_or_default();
        lines.push(format!(
            "第{}章 {}｜强度{}｜钩-{}｜{}字｜{}",
            m.idx,
            m.title,
            if m.intensity > 0 {
                m.intensity.to_string()
            } else {
                "未标注".into()
            },
            if m.hook_type.trim().is_empty() {
                "未标注"
            } else {
                m.hook_type.trim()
            },
            m.words,
            sum
        ));
        if let Some(ch) = engine.db.get_chapter(novel_id, m.idx)? {
            if ch.status == "done" && !ch.content.trim().is_empty() {
                contents.push((m.idx, ch.content));
            }
        }
    }
    if contents.is_empty() {
        // 该卷还没有已写正文（只规划）：不体检
        return Ok(());
    }
    let stats = crate::style_stats::compute(
        &contents
            .iter()
            .map(|(i, c)| (*i, c.as_str()))
            .collect::<Vec<_>>(),
    );
    let volume_summary = engine
        .db
        .list_volume_summaries(novel_id)?
        .iter()
        .find(|v| v.volume_idx == volume_idx)
        .map(|v| v.summary.clone())
        .unwrap_or_default();
    let fores: Vec<String> = engine
        .db
        .list_foreshadowings(novel_id)?
        .iter()
        .filter(|f| f.status == "open")
        .map(|f| {
            if f.planted_chapter > 0 {
                format!(
                    "{}（第{}章埋下，拖 {} 章）",
                    f.title,
                    f.planted_chapter,
                    (to - f.planted_chapter).max(0)
                )
            } else {
                format!("{}（手动记录）", f.title)
            }
        })
        .collect();
    engine.hub.publish(novel_id, EngineEvent::Stage {
        stage: "extract".into(),
        message: format!("第 {volume_idx} 卷体检（第 {from}~{to} 章）…"),
    });
    let settings = engine.llm_settings()?;
    let llm = Llm::new(settings.clone());
    let ov = engine.prompt_overrides();
    let (sys, usr) = crate::prompts::volume_review_prompt(
        volume_idx,
        from,
        to,
        &volume_summary,
        &lines.join("\n"),
        &fores.join("；"),
        &stats.summary_text,
        &ov,
    );
    let out = llm.complete(&sys, &usr).await?;
    record_usage(
        &engine.db,
        Some(novel_id),
        None,
        "volume_review",
        &settings,
        &out.usage,
    );
    let report = extract_json_object(&out.text).unwrap_or_else(|| {
        serde_json::json!({ "ok": true, "findings": [], "note": "输出无法解析（已跳过）" })
    });
    let findings = report["findings"].as_array().map(|a| a.len()).unwrap_or(0);
    engine
        .db
        .upsert_volume_report(novel_id, volume_idx, to, &serde_json::to_string(&report)?)?;
    engine.hub.publish(novel_id, EngineEvent::Stage {
        stage: "extract".into(),
        message: if findings > 0 {
            format!("第 {volume_idx} 卷体检完成：{findings} 条建议（可在「书籍体检」查看）")
        } else {
            format!("第 {volume_idx} 卷体检完成：无明显问题")
        },
    });
    Ok(())
}

/// 卷末体检（含自我优化闭环）：体检 → 高/中危发现定向修复 → 复检重出报告（一轮）。
/// 已有报告则跳过（章节改动会让报告失效，届时自动重跑）。
async fn generate_volume_review(
    engine: &Arc<Engine>,
    novel_id: &str,
    volume_idx: i64,
) -> anyhow::Result<()> {
    if engine.db.has_volume_report(novel_id, volume_idx)? {
        return Ok(());
    }
    volume_review_once(engine, novel_id, volume_idx).await?;
    // 自我优化：把体检发现的高/中危问题定向修复（复用全局审校修复管线），然后复检重出报告
    let settings = engine.db.pipeline_settings().unwrap_or_default();
    if !settings.auto_fix_global {
        return Ok(());
    }
    let report = engine
        .db
        .list_volume_reports(novel_id)?
        .into_iter()
        .find(|(v, _, _)| *v == volume_idx)
        .map(|(_, _, js)| js)
        .unwrap_or_default();
    let findings = extract_json_object(&report)
        .and_then(|v| v["findings"].as_array().cloned())
        .unwrap_or_default();
    let targets: Vec<GlobalIssue> = findings
        .iter()
        .filter_map(|f| {
            let severity = f["severity"].as_str().unwrap_or("low").to_string();
            if severity != "high" && severity != "medium" {
                return None;
            }
            let chapters: Vec<i64> = f["chapters"]
                .as_array()
                .map(|a| a.iter().filter_map(|x| x.as_i64()).collect())
                .unwrap_or_default();
            if chapters.is_empty() {
                return None;
            }
            Some(GlobalIssue {
                severity,
                note: f["note"].as_str().unwrap_or("").trim().to_string(),
                affected_chapters: chapters,
                suggestion: f["suggestion"].as_str().unwrap_or("").trim().to_string(),
                status: "open".into(),
                is_new: false,
            })
        })
        .take(2)
        .collect();
    if targets.is_empty() {
        return Ok(());
    }
    let style_id = engine
        .db
        .get_novel(novel_id)?
        .map(|n| n.style_preset)
        .unwrap_or_default();
    let style = crate::styles::find(&style_id);
    let ov = engine.prompt_overrides();
    let mut fixed = 0usize;
    for issue in &targets {
        if engine.stop_requested(novel_id) {
            break;
        }
        if fix_chapter_by_global_issue(engine, novel_id, issue, style, &ov)
            .await
            .unwrap_or(false)
        {
            fixed += 1;
        }
    }
    if fixed > 0 {
        // 复检：重出本卷报告（反映修复后的最新情况）
        engine.db.delete_volume_report(novel_id, volume_idx)?;
        if let Err(e) = volume_review_once(engine, novel_id, volume_idx).await {
            tracing::warn!("volume re-review {volume_idx} failed: {e:#}");
        }
        engine.hub.publish(novel_id, EngineEvent::Stage {
            stage: "extract".into(),
            message: format!("第 {volume_idx} 卷体检：自动修复 {fixed} 处并复检完成"),
        });
    }
    Ok(())
}

/// 补写缺失的卷级摘要：覆盖「第 20/40… 章由人工或单章接口完成」导致跳过边界触发的情况。
async fn maybe_fill_volume_summaries(
    engine: &Arc<Engine>,
    novel_id: &str,
    upto: i64,
) -> anyhow::Result<usize> {
    let max_vol = upto / VOLUME_SPAN;
    if max_vol <= 0 {
        return Ok(0);
    }
    let existing: std::collections::HashSet<i64> = engine
        .db
        .list_volume_summaries(novel_id)?
        .iter()
        .map(|v| v.volume_idx)
        .collect();
    let mut made = 0usize;
    for v in 1..=max_vol {
        if existing.contains(&v) {
            continue;
        }
        match generate_volume_summary(engine, novel_id, v).await {
            Ok(()) => made += 1,
            Err(e) => tracing::warn!("volume summary {v} failed: {e:#}"),
        }
    }
    // 卷末体检补跑：摘要已存在但缺报告的卷（有界：每卷一次，内部自带去重检查）
    for v in 1..=max_vol {
        if let Err(e) = generate_volume_review(engine, novel_id, v).await {
            tracing::warn!("volume review {v} failed: {e:#}");
        }
    }
    Ok(made)
}

// ---------- 读者视角评审与完本收尾 ----------

/// 读者视角评审（一次 LLM 调用）：指定区间的目标 / 摘要 / 章末三行 → (覆盖章数, 评审 JSON)。
/// 供 API 手动调用与完本收尾复用；「读者审查」只在完本收尾里自动执行（写作过程中不打扰）。
pub async fn reader_review_once(
    engine: &Arc<Engine>,
    novel_id: &str,
    from: i64,
    to: i64,
    golden: bool,
) -> anyhow::Result<(usize, serde_json::Value)> {
    let novel = engine
        .db
        .get_novel(novel_id)?
        .ok_or_else(|| anyhow::anyhow!("novel not found: {novel_id}"))?;
    let from = from.max(1);
    let to = to.max(from).min(from + 19);
    let mut text = String::new();
    let mut included = 0usize;
    for idx in from..=to {
        let Some(ch) = engine.db.get_chapter(novel_id, idx)? else {
            continue;
        };
        if ch.status != "done" || ch.content.trim().is_empty() {
            continue;
        }
        let tail = ch
            .content
            .lines()
            .rev()
            .filter(|l| !l.trim().is_empty())
            .take(3)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect::<Vec<_>>()
            .join("\n");
        let summary: String = ch.summary.trim().chars().take(60).collect();
        text.push_str(&format!(
            "【第{}章 {}】目标：{}\n摘要：{}\n章末三行：\n{}\n\n",
            ch.idx, ch.title, ch.goal, summary, tail
        ));
        included += 1;
    }
    if included == 0 {
        anyhow::bail!("该区间还没有已写正文的章节");
    }
    let brief = Brief {
        title: novel.title.clone(),
        genre: novel.genre.clone(),
        logline: novel.logline.clone(),
        protagonist: novel.protagonist.clone(),
        style: novel.style.clone(),
        worldview: novel.worldview.clone(),
    };
    let settings = engine.llm_settings()?;
    let llm = Llm::new(settings.clone());
    let ov = engine.prompt_overrides();
    let (sys, usr) = crate::prompts::reader_review_prompt(
        &brief,
        from,
        to,
        golden,
        &text,
        crate::styles::find(&novel.style_preset),
        &ov,
    );
    let out = llm.complete(&sys, &usr).await?;
    record_usage(
        &engine.db,
        Some(novel_id),
        None,
        "reader_review",
        &settings,
        &out.usage,
    );
    let result = extract_json_object(&out.text)
        .ok_or_else(|| anyhow::anyhow!("读者评审输出不是 JSON（可重试一次）"))?;
    Ok((included, result))
}

/// 完本收尾（自动：目标达成时触发；或手动重跑）：
/// 巡检 → 全局审校（自动定向修复）→ 卷摘要/卷末体检补齐（含修复复检）→
/// 读者审查（黄金三章 + 每 20 章分块）→ 完本报告存 book_reports。
/// 返回是否已启动（false = 已收尾过或已有审校/收尾在进行）。
pub fn spawn_finish_book(engine: Arc<Engine>, novel_id: &str, force: bool) -> anyhow::Result<bool> {
    if !force && engine.db.has_book_report(novel_id, "finish").unwrap_or(false) {
        return Ok(false); // 已收尾过（章节改动会让报告失效，届时自动重跑）
    }
    if !engine.begin_review(novel_id) {
        return Ok(false); // 已有审校/收尾在进行：跳过
    }
    let slot = ReviewSlot {
        engine: engine.clone(),
        novel_id: novel_id.to_string(),
    };
    let nid = novel_id.to_string();
    tokio::spawn(async move {
        let _slot = slot;
        if let Err(e) = finish_book_inner(&engine, &nid).await {
            tracing::warn!("finish book failed: {e:#}");
            engine.hub.publish(&nid, EngineEvent::Stage {
                stage: "done".into(),
                message: format!("完本收尾失败：{e:#}"),
            });
        }
    });
    Ok(true)
}

async fn finish_book_inner(engine: &Arc<Engine>, novel_id: &str) -> anyhow::Result<()> {
    engine.hub.publish(novel_id, EngineEvent::Stage {
        stage: "finish".into(),
        message: "完本收尾：开工巡检与状态整理…".into(),
    });
    let _ = maintain_novel(engine, novel_id, 3, 1).await;
    engine.hub.publish(novel_id, EngineEvent::Stage {
        stage: "finish".into(),
        message: "完本收尾：全局审校（含自动定向修复）…".into(),
    });
    let _ = global_review_and_fix(engine, novel_id, 3).await;
    let last_done = engine
        .db
        .list_chapter_metas(novel_id)
        .map(|cs| {
            cs.iter()
                .filter(|c| c.status == "done")
                .map(|c| c.idx)
                .max()
                .unwrap_or(0)
        })
        .unwrap_or(0);
    let _ = maybe_fill_volume_summaries(engine, novel_id, last_done).await;
    // 读者审查：黄金三章 + 每 20 章分块（只在完本时自动执行）
    let mut golden: Option<serde_json::Value> = None;
    let mut blocks: Vec<serde_json::Value> = Vec::new();
    if last_done >= 1 {
        if last_done >= 3 {
            engine.hub.publish(novel_id, EngineEvent::Stage {
                stage: "finish".into(),
                message: "完本收尾：读者审查（黄金三章）…".into(),
            });
            match reader_review_once(engine, novel_id, 1, 3, true).await {
                Ok((_n, v)) => golden = Some(v),
                Err(e) => tracing::warn!("finish golden reader review failed: {e:#}"),
            }
        }
        let mut from = 1i64;
        while from <= last_done {
            let to = (from + 19).min(last_done);
            engine.hub.publish(novel_id, EngineEvent::Stage {
                stage: "finish".into(),
                message: format!("完本收尾：读者审查 第 {from}~{to} 章…"),
            });
            match reader_review_once(engine, novel_id, from, to, false).await {
                Ok((_n, mut v)) => {
                    if let Some(o) = v.as_object_mut() {
                        o.insert("from".into(), serde_json::json!(from));
                        o.insert("to".into(), serde_json::json!(to));
                    }
                    blocks.push(v);
                }
                Err(e) => tracing::warn!("finish reader review {from}-{to} failed: {e:#}"),
            }
            from = to + 1;
        }
    }
    let blocks_n = blocks.len();
    let report = serde_json::json!({
        "finished_at": chrono::Local::now().to_rfc3339(),
        "chapters": last_done,
        "golden": golden,
        "blocks": blocks,
    });
    engine
        .db
        .upsert_book_report(novel_id, "finish", &serde_json::to_string(&report)?)?;
    engine.db.set_novel_status(novel_id, "finished")?;
    engine.hub.publish(novel_id, EngineEvent::Stage {
        stage: "done".into(),
        message: format!(
            "完本收尾完成：读者审查已生成（{last_done} 章；黄金三章 + {blocks_n} 个分块），书籍体检 → 完本报告查看"
        ),
    });
    Ok(())
}

// ---------- 章节版本历史与 AI 复原 ----------

/// 一版正文的综合评分（本地、零 token）：文风分 -（未解决的中/高危意见 × 20）- 篇幅塌陷罚分
#[derive(Debug, Clone, Serialize)]
pub struct SnapshotScore {
    pub words: i64,
    pub open_issues: usize,
    pub style: Option<i64>,
    pub score: i64,
}

pub fn score_snapshot(
    content: &str,
    review_json: Option<&str>,
    style_report: &str,
    max_words: i64,
) -> SnapshotScore {
    let words = content.chars().count() as i64;
    let open_issues = review_json
        .and_then(|raw| serde_json::from_str::<Review>(raw).ok())
        .map(|r| {
            r.issues
                .iter()
                .filter(|i| !is_style_issue(i))
                .filter(|i| i.status != "fixed")
                .filter(|i| i.severity == "high" || i.severity == "medium")
                .count()
        })
        .unwrap_or(0);
    let style: Option<i64> = serde_json::from_str::<crate::style_check::StyleReport>(style_report)
        .ok()
        .map(|r| r.score() as i64);
    let mut score = style.unwrap_or(60);
    score -= (open_issues as i64) * 20;
    if max_words > 0 && words * 100 < max_words * 40 {
        score -= 25; // 不足最长版本 40%：更像异常截断而不是有意精简
    }
    SnapshotScore {
        words,
        open_issues,
        style,
        score,
    }
}

/// AI 复原：在当前稿与历史快照之间选「综合最优」；只有严格更优才恢复（本地评分，零 token）。
/// 返回 Some(说明) 表示发生了一次复原，事件流里会看到「AI 复原」提示。
pub async fn ai_restore_chapter(
    engine: &Arc<Engine>,
    novel_id: &str,
    idx: i64,
    via: &str,
) -> anyhow::Result<Option<String>> {
    let Some(ch) = engine.db.get_chapter(novel_id, idx)? else {
        return Ok(None);
    };
    if ch.content.trim().is_empty() {
        return Ok(None);
    }
    let versions = engine.db.list_chapter_versions(novel_id, idx)?;
    if versions.is_empty() {
        return Ok(None);
    }
    let max_words = versions
        .iter()
        .map(|v| v.content.chars().count() as i64)
        .chain([ch.content.chars().count() as i64])
        .max()
        .unwrap_or(0);
    let cur = score_snapshot(&ch.content, ch.review_json.as_deref(), &ch.style_report, max_words);
    let mut best: Option<(&crate::models::ChapterVersion, SnapshotScore)> = None;
    for v in &versions {
        if v.content.trim().is_empty() {
            continue;
        }
        let s = score_snapshot(&v.content, v.review_json.as_deref(), &v.style_report, max_words);
        match &best {
            Some((_, bs)) if bs.score >= s.score => {}
            _ => best = Some((v, s)),
        }
    }
    let Some((v, vs)) = best else {
        return Ok(None);
    };
    if vs.score <= cur.score {
        return Ok(None);
    }
    if !engine
        .db
        .restore_chapter_version(novel_id, idx, &v.id, via)?
    {
        return Ok(None);
    }
    engine.hub.publish(novel_id, EngineEvent::ChapterSaved {
        chapter_idx: idx,
        title: ch.title.clone(),
        words: vs.words.max(0) as usize,
    });
    let why = format!(
        "问题 {} → {} 条，评分 {} → {}（快照 {}）",
        cur.open_issues,
        vs.open_issues,
        cur.score,
        vs.score,
        v.created_at.chars().take(16).collect::<String>()
    );
    engine.hub.publish(novel_id, EngineEvent::Stage {
        stage: "autofix".into(),
        message: format!("AI 复原：第 {idx} 章已恢复更优快照（{why}）"),
    });
    Ok(Some(why))
}

// ---------- 开工巡检（每批自动 + 手动端点） ----------

/// 内容指纹（FNV-1a 64）：审校时记录，正文一旦被改（手动编辑/单章操作）即可被巡检识别为「漏审」
pub fn content_hash(s: &str) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in s.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{h:016x}")
}

/// 漏审候选：正文在审校之后被改过、且仍有未解决高/中危意见的定稿章节。
/// 旧数据（无指纹）跳过，避免升级后全量重审。
pub fn stale_review_candidates(
    db: &crate::db::Db,
    novel_id: &str,
    limit: usize,
) -> Vec<(i64, String, String)> {
    let Ok(chapters) = db.list_chapters(novel_id) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for c in chapters.iter() {
        if c.status != "done" || c.content.trim().is_empty() {
            continue;
        }
        let Some(r) = c
            .review_json
            .as_deref()
            .and_then(|raw| serde_json::from_str::<Review>(raw).ok())
        else {
            continue;
        };
        if r.content_hash.is_empty() || r.content_hash == content_hash(&c.content) {
            continue;
        }
        if medium_high_count(&r) == 0 {
            continue;
        }
        out.push((c.idx, c.title.clone(), c.goal.clone()));
        if out.len() >= limit {
            break;
        }
    }
    out
}

/// 解析立项澄清问答（宽容：畸形 JSON 视为无澄清）
pub fn parse_clarify(raw: &str) -> Vec<crate::models::ClarifyQA> {
    serde_json::from_str::<Vec<crate::models::ClarifyQA>>(raw).unwrap_or_default()
}

/// 开工巡检结果（全部为「本次实际做了多少」）
#[derive(Debug, Default, Clone, Serialize)]
pub struct MaintenanceReport {
    /// done 但无正文的章节：状态收回 planned（UI 不再误显示已完成）
    pub fixed_empty_status: usize,
    pub filled_summaries: usize,
    pub missing_summaries_left: usize,
    pub revisited: usize,
    pub stale_left: usize,
}

impl MaintenanceReport {
    pub fn touched(&self) -> usize {
        self.fixed_empty_status + self.filled_summaries + self.revisited
    }
}

/// 开工巡检：本地状态修正 + 缺摘要补齐（bounded）+ 漏审章节重审（bounded）。
/// 每批运行开始自动跑轻量版（3/1）；手动端点跑全量版（10/3）。
pub async fn maintain_novel(
    engine: &Arc<Engine>,
    novel_id: &str,
    summary_limit: usize,
    revisit_limit: usize,
) -> anyhow::Result<MaintenanceReport> {
    let mut report = MaintenanceReport::default();
    let chapters = engine.db.list_chapters(novel_id)?;

    // ① 状态修正：done 但无正文（此前抽取/落库中断造成），收回 planned 交给下一轮重写
    for c in chapters.iter() {
        if c.status == "done" && c.content.trim().is_empty() {
            engine.db.set_chapter_status(novel_id, c.idx, "planned")?;
            report.fixed_empty_status += 1;
        }
    }

    // ② 缺摘要补齐（有正文、无摘要；影响后续章节的上下文注入质量）
    let missing: Vec<&crate::models::Chapter> = chapters
        .iter()
        .filter(|c| !c.content.trim().is_empty() && c.summary.trim().is_empty())
        .collect();
    if !missing.is_empty() && summary_limit > 0 {
        let settings = engine.llm_settings()?;
        let llm = Llm::new(settings.clone());
        let ov = engine.prompt_overrides();
        for c in missing.iter().take(summary_limit) {
            if engine.stop_requested(novel_id) {
                // 保留停止标记不清：让随后运行的章节循环在检查点优雅收尾
                break;
            }
            let (sys, usr) = crate::prompts::extract_prompt(c.idx, &c.content, &ov);
            match llm.complete(&sys, &usr).await {
                Ok(out) => {
                    record_usage(
                        &engine.db,
                        Some(novel_id),
                        Some(c.idx),
                        "extract",
                        &settings,
                        &out.usage,
                    );
                    if let Some(j) = extract_json_object(&out.text) {
                        apply_extraction(&engine.db, novel_id, c.idx, &j);
                        report.filled_summaries += 1;
                    }
                }
                Err(e) => tracing::warn!("maintain extract chapter {} failed: {e:#}", c.idx),
            }
        }
    }
    report.missing_summaries_left = missing.len().saturating_sub(report.filled_summaries);

    // ③ 漏审修复：正文在审校后被改过且仍有高/中危意见（手动编辑/AI 单章操作会命中）
    let candidates = stale_review_candidates(&engine.db, novel_id, 1000);
    if !candidates.is_empty() && revisit_limit > 0 {
        let style_id = engine
            .db
            .get_novel(novel_id)?
            .map(|n| n.style_preset)
            .unwrap_or_default();
        let style = crate::styles::find(&style_id);
        let ov = engine.prompt_overrides();
        for (idx, title, goal) in candidates.iter().take(revisit_limit) {
            if engine.stop_requested(novel_id) {
                break;
            }
            let Some(ch) = engine.db.get_chapter(novel_id, *idx)? else {
                continue;
            };
            engine.hub.publish(novel_id, EngineEvent::Stage {
                stage: "review".into(),
                message: format!("开工巡检：复审第 {idx} 章（正文在审校后被改动过）…"),
            });
            let mut content = ch.content.clone();
            match review_and_fix_chapter(engine, novel_id, *idx, title, goal, &mut content, style, &ov)
                .await
            {
                Ok(_) => report.revisited += 1,
                Err(e) => tracing::warn!("maintain revisit chapter {idx} failed: {e:#}"),
            }
            if ch.status == "done" {
                let _ =
                    engine
                        .db
                        .save_chapter_content_sourced(novel_id, *idx, &content, "done", "maintain");
            }
        }
    }
    report.stale_left = candidates.len().saturating_sub(report.revisited);
    Ok(report)
}

/// 手动「开工巡检」：后台执行（占审校名额），完成后推事件汇总
pub fn spawn_maintenance(engine: Arc<Engine>, novel_id: &str) -> anyhow::Result<()> {
    if !engine.begin_review(novel_id) {
        anyhow::bail!("已有一轮审校处理在进行中");
    }
    let slot = ReviewSlot {
        engine: engine.clone(),
        novel_id: novel_id.to_string(),
    };
    let nid = novel_id.to_string();
    tokio::spawn(async move {
        let _slot = slot;
        match maintain_novel(&engine, &nid, 10, 3).await {
            Ok(r) => {
                engine.hub.publish(&nid, EngineEvent::Stage {
                    stage: "done".into(),
                    message: format!(
                        "开工巡检完成：修正 {} 处状态，补 {} 条摘要，重审 {} 章{}",
                        r.fixed_empty_status,
                        r.filled_summaries,
                        r.revisited,
                        if r.stale_left > 0 {
                            format!("（仍余 {} 章待重审，可点「AI 处理全部审校」）", r.stale_left)
                        } else {
                            String::new()
                        }
                    ),
                });
            }
            Err(e) => {
                engine.hub.publish(&nid, EngineEvent::Stage {
                    stage: "done".into(),
                    message: format!("开工巡检失败：{e:#}"),
                });
            }
        }
    });
    Ok(())
}

// ---------- 全局（跨章）审校 ----------

/// 全局审校意见的跨轮归并：文案相似视为同一条，不再出现的标 fixed，多出来的标「新发现」
pub fn merge_global_issues(previous: &[GlobalIssue], latest: &[GlobalIssue]) -> Vec<GlobalIssue> {
    let mut out: Vec<GlobalIssue> = Vec::new();
    let mut matched: Vec<bool> = vec![false; latest.len()];
    for p in previous {
        if p.status == "fixed" {
            out.push(p.clone());
            continue;
        }
        let hit = latest
            .iter()
            .enumerate()
            .find(|(i, l)| !matched[*i] && notes_similar(&p.note, &l.note));
        match hit {
            Some((i, l)) => {
                matched[i] = true;
                out.push(GlobalIssue {
                    status: "open".into(),
                    is_new: false,
                    ..l.clone()
                });
            }
            None => out.push(GlobalIssue {
                status: "fixed".into(),
                ..p.clone()
            }),
        }
    }
    for (i, l) in latest.iter().enumerate() {
        if !matched[i] {
            out.push(GlobalIssue {
                status: "open".into(),
                is_new: true,
                ..l.clone()
            });
        }
    }
    out
}

/// 跑一次全局审校（全书视角）：设定 + 卷摘要 + 逐章摘要 + 台账 → 跨章问题（含跨轮状态归并）
pub async fn run_global_review(
    engine: &Arc<Engine>,
    novel_id: &str,
) -> anyhow::Result<GlobalReview> {
    let novel = engine
        .db
        .get_novel(novel_id)?
        .ok_or_else(|| anyhow::anyhow!("novel not found: {novel_id}"))?;
    let summaries_list = engine.db.list_chapter_summaries(novel_id)?;
    let with_summary = summaries_list
        .iter()
        .filter(|(_, _, s)| !s.trim().is_empty())
        .count();
    if with_summary < 3 {
        // 章节太少，跨章问题没有统计意义
        return Ok(GlobalReview {
            ok: true,
            issues: Vec::new(),
            updated_at: chrono::Utc::now().to_rfc3339(),
        });
    }
    let lines: Vec<String> = summaries_list
        .iter()
        .filter(|(_, _, s)| !s.trim().is_empty())
        .map(|(idx, title, summary)| format!("第{}章 {}：{}", idx, title, summary.trim()))
        .collect();
    let summaries_text = tail_chars(&lines.join("\n"), 6000);
    let volumes = engine.db.list_volume_summaries(novel_id)?;
    let vol_text = volumes
        .iter()
        .rev()
        .take(5)
        .rev()
        .map(|v| format!("第{}卷（至第{}章）：{}", v.volume_idx, v.upto_chapter, v.summary.trim()))
        .collect::<Vec<_>>()
        .join("\n");
    let last_idx = summaries_list.iter().map(|(i, _, _)| *i).max().unwrap_or(0);
    let ledger = ledger_context(&engine.db, novel_id, last_idx + 1).unwrap_or_default();
    engine.hub.publish(novel_id, EngineEvent::Stage {
        stage: "review".into(),
        message: "全局审校：正在检查跨章一致性、节奏与支线…".into(),
    });
    let settings = engine.llm_settings()?;
    let llm = Llm::new(settings.clone());
    let ov = engine.prompt_overrides();
    let (sys, usr) = crate::prompts::global_review_prompt(
        &novel.title,
        &novel.genre,
        &novel.logline,
        &novel.worldview,
        with_summary as i64,
        &vol_text,
        &summaries_text,
        &ledger,
        &ov,
    );
    let out = llm.complete(&sys, &usr).await?;
    record_usage(&engine.db, Some(novel_id), None, "global_review", &settings, &out.usage);
    let raw = out.text;
    let parsed = extract_json_object(&raw)
        .filter(|j| j["issues"].is_array())
        .map(|j| GlobalReview {
            ok: j["ok"].as_bool().unwrap_or(true),
            issues: j["issues"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .map(|it| GlobalIssue {
                            severity: str_field(it, "severity", "info"),
                            note: str_field(it, "note", ""),
                            affected_chapters: it["affected_chapters"]
                                .as_array()
                                .map(|x| x.iter().filter_map(|v| v.as_i64()).collect())
                                .unwrap_or_default(),
                            suggestion: str_field(it, "suggestion", ""),
                            status: "open".into(),
                            is_new: false,
                        })
                        .collect()
                })
                .unwrap_or_default(),
            updated_at: String::new(),
        })
        .ok_or_else(|| anyhow::anyhow!("全局审校输出无法解析（缺 issues 数组）"))?;
    let previous = engine
        .db
        .get_global_review(novel_id)?
        .map(|g| g.issues)
        .unwrap_or_default();
    let review = GlobalReview {
        ok: parsed.ok,
        issues: merge_global_issues(&previous, &parsed.issues),
        updated_at: chrono::Utc::now().to_rfc3339(),
    };
    engine.db.save_global_review(novel_id, &review)?;
    Ok(review)
}

/// 按一条全局意见定向修复一个受影响章节：修复 → 章节级复核 → 若综合分反而更差则 AI 复原
async fn fix_chapter_by_global_issue(
    engine: &Arc<Engine>,
    novel_id: &str,
    issue: &GlobalIssue,
    style: Option<&crate::styles::StylePreset>,
    ov: &crate::prompts::Overrides,
) -> anyhow::Result<bool> {
    let settings = engine.llm_settings()?;
    let llm = Llm::new(settings.clone());
    for idx in &issue.affected_chapters {
        let Some(ch) = engine.db.get_chapter(novel_id, *idx)? else {
            continue;
        };
        if ch.content.trim().is_empty() {
            continue;
        }
        let instruction = format!(
            "全局审校意见（跨章问题）：{}。修改建议：{}",
            issue.note.trim(),
            issue.suggestion.trim()
        );
        engine.hub.publish(novel_id, EngineEvent::Stage {
            stage: "autofix".into(),
            message: format!("全局审校：按意见修复第 {idx} 章…"),
        });
        let (fsys, fusr) = crate::prompts::rewrite_prompt(
            "revise",
            &ch.title,
            &ch.goal,
            &ch.content,
            &instruction,
            style,
            ov,
        );
        match llm.complete(&fsys, &fusr).await {
            Ok(out) => {
                record_usage(
                    &engine.db,
                    Some(novel_id),
                    Some(*idx),
                    "global_fix",
                    &settings,
                    &out.usage,
                );
                let new = out.text.trim().to_string();
                let old_words = ch.content.chars().count();
                if new.is_empty() || new == ch.content || new.chars().count() * 2 < old_words {
                    tracing::warn!("global fix chapter {idx}: 结果为空/等长/疑似截断，放弃");
                    continue;
                }
                engine
                    .db
                    .save_chapter_content_sourced(novel_id, *idx, &new, "done", "global-fix")?;
                // 章节级复核（含自动修复）
                let mut content = new.clone();
                if let Err(e) = review_and_fix_chapter(
                    engine, novel_id, *idx, &ch.title, &ch.goal, &mut content, style, ov,
                )
                .await
                {
                    tracing::warn!("chapter {idx} post-global-fix review failed: {e:#}");
                }
                if ch.status == "done" {
                    let _ = engine
                        .db
                        .save_chapter_content_sourced(novel_id, *idx, &content, "done", "global-fix");
                }
                // 若修复后综合分反而更差，交给 AI 复原（恢复修复前快照）
                let _ = ai_restore_chapter(engine, novel_id, *idx, "ai-restore").await;
                return Ok(true);
            }
            Err(e) => {
                tracing::warn!("global fix chapter {idx} failed: {e:#}");
                return Ok(false);
            }
        }
    }
    Ok(false)
}

/// 全局审校 + 自动定向修复：修复过至少一章后复检一次（状态自动归并）
pub async fn global_review_and_fix(
    engine: &Arc<Engine>,
    novel_id: &str,
    fix_limit: usize,
) -> anyhow::Result<(GlobalReview, usize)> {
    let review = run_global_review(engine, novel_id).await?;
    let settings = engine.db.pipeline_settings().unwrap_or_default();
    let targets: Vec<GlobalIssue> = if settings.auto_fix_global && fix_limit > 0 {
        review
            .issues
            .iter()
            .filter(|i| {
                i.status == "open"
                    && (i.severity == "high" || i.severity == "medium")
                    && !i.affected_chapters.is_empty()
            })
            .take(fix_limit)
            .cloned()
            .collect()
    } else {
        Vec::new()
    };
    if targets.is_empty() {
        return Ok((review, 0));
    }
    let style_id = engine
        .db
        .get_novel(novel_id)?
        .map(|n| n.style_preset)
        .unwrap_or_default();
    let style = crate::styles::find(&style_id);
    let ov = engine.prompt_overrides();
    let mut fixed = 0usize;
    for issue in &targets {
        if engine.stop_requested(novel_id) {
            engine.clear_stop(novel_id);
            break;
        }
        if fix_chapter_by_global_issue(engine, novel_id, issue, style, &ov)
            .await
            .unwrap_or(false)
        {
            fixed += 1;
        }
    }
    if fixed == 0 {
        return Ok((review, 0));
    }
    // 复检一次：消失的意见自动转 fixed，仍存在的保持 open（「新发现」只给真正的新问题）
    let refreshed = run_global_review(engine, novel_id).await?;
    Ok((refreshed, fixed))
}

/// 后台运行一次全局审校（含自动修复）；占用与「一键处理」同一名额，结束后自动释放
pub fn spawn_global_review(engine: Arc<Engine>, novel_id: &str) -> anyhow::Result<()> {
    if !engine.begin_review(novel_id) {
        anyhow::bail!("已有一轮审校处理在进行中");
    }
    let slot = ReviewSlot {
        engine: engine.clone(),
        novel_id: novel_id.to_string(),
    };
    let nid = novel_id.to_string();
    tokio::spawn(async move {
        let _slot = slot;
        match global_review_and_fix(&engine, &nid, 3).await {
            Ok((r, fixed)) => {
                let open = r.issues.iter().filter(|i| i.status != "fixed").count();
                engine.hub.publish(&nid, EngineEvent::Stage {
                    stage: "done".into(),
                    message: format!(
                        "全局审校完成：共 {} 条意见（仍存在 {}），自动修复 {fixed} 章",
                        r.issues.len(),
                        open
                    ),
                });
            }
            Err(e) => {
                engine.hub.publish(&nid, EngineEvent::Stage {
                    stage: "done".into(),
                    message: format!("全局审校失败：{e:#}"),
                });
            }
        }
        // G3：审校收尾后释放名额，若自动连载仍未达标则自动续批
        drop(_slot);
        let _ = maybe_chain_auto(&engine, &nid);
    });
    Ok(())
}

/// 后台按指定的一条全局意见定向修复（手动触发）；结束前复检刷新状态
pub fn spawn_global_issue_fix(
    engine: Arc<Engine>,
    novel_id: &str,
    index: usize,
) -> anyhow::Result<()> {
    if !engine.begin_review(novel_id) {
        anyhow::bail!("已有一轮审校处理在进行中");
    }
    let slot = ReviewSlot {
        engine: engine.clone(),
        novel_id: novel_id.to_string(),
    };
    let nid = novel_id.to_string();
    tokio::spawn(async move {
        let _slot = slot;
        let issue = engine
            .db
            .get_global_review(&nid)
            .ok()
            .flatten()
            .and_then(|r| r.issues.get(index).cloned());
        let Some(issue) = issue else {
            engine.hub.publish(&nid, EngineEvent::Stage {
                stage: "done".into(),
                message: "全局审校：未找到该条意见（可能已刷新）".into(),
            });
            return;
        };
        let style_id = engine
            .db
            .get_novel(&nid)
            .ok()
            .flatten()
            .map(|n| n.style_preset)
            .unwrap_or_default();
        let style = crate::styles::find(&style_id);
        let ov = engine.prompt_overrides();
        match fix_chapter_by_global_issue(&engine, &nid, &issue, style, &ov).await {
            Ok(true) => {
                let _ = run_global_review(&engine, &nid).await;
                engine.hub.publish(&nid, EngineEvent::Stage {
                    stage: "done".into(),
                    message: "全局审校：该意见的定向修复已完成（状态已刷新）".into(),
                });
            }
            Ok(false) => {
                engine.hub.publish(&nid, EngineEvent::Stage {
                    stage: "done".into(),
                    message: "全局审校：该意见暂无可修复的章节（可能未指明章节或章节为空）".into(),
                });
            }
            Err(e) => {
                engine.hub.publish(&nid, EngineEvent::Stage {
                    stage: "done".into(),
                    message: format!("全局审校修复失败：{e:#}"),
                });
            }
        }
    });
    Ok(())
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
                // 必须带 issues 数组：缺字段视为解析失败（否则会被当成「0 条问题」误放行）
                if let Some(j) = extract_json_object(&raw).filter(|j| j["issues"].is_array()) {
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
                                        status: "open".to_string(),
                                        is_new: false,
                                        stale: false,
                                    })
                                    .collect()
                            })
                            .unwrap_or_default(),
                        content_hash: String::new(),
                    };
                    // 校验引用可定位性：模型引用不逐字 / 稿子改动后，标记 stale（UI 会提示）
                    mark_quote_flags(&content, &mut review.issues);

                    // ---- 自动修复（减少人工介入）----
                    // 审校出现高/中危问题时：按意见自动修订一次并复核，仅在问题变少时采用新稿。
                    let pipeline_settings = engine.db.pipeline_settings().unwrap_or_default();
                    let mut fixable: Vec<String> = review
                        .issues
                        .iter()
                        .filter(|i| i.severity == "high" || i.severity == "medium")
                        .map(|i| i.note.trim().to_string())
                        .filter(|n| !n.is_empty())
                        .collect();
                    // 本地文风硬指标一并交给这次修复处理（修完会重算文风报告）
                    if let Some(r) = &report {
                        for c in r.checks.iter().filter(|c| c.hard && !c.passed) {
                            fixable.push(format!("文风检查：{} —— {}", c.name, c.detail));
                        }
                    }
                    let fixable: Vec<String> = fixable.into_iter().take(8).collect();
                    if pipeline_settings.auto_fix_review
                        && !fixable.is_empty()
                        && !engine.stop_requested(novel_id)
                    {
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
                                                extract_json_object(&r2out.text).and_then(|j2| {
                                                    j2["issues"].as_array().map(|a| {
                                                        a.iter()
                                                            .map(|it| ReviewIssue {
                                                                severity: str_field(
                                                                    it, "severity", "info",
                                                                ),
                                                                note: str_field(it, "note", ""),
                                                                quote: str_field(it, "quote", ""),
                                                                status: "open".to_string(),
                                                                is_new: false,
                                                                stale: false,
                                                            })
                                                            .collect()
                                                    })
                                                })
                                            }
                                            Err(e) => {
                                                tracing::warn!(
                                                    "chapter {idx} re-review failed: {e:#}"
                                                );
                                                None
                                            }
                                        };
                                    // 篇幅兜底：修复稿不足原稿一半视为疑似截断，不采纳（版本快照仍保留可回溯）
                                    let words_ok =
                                        fixed.chars().count() * 2 >= content.chars().count().max(1);
                                    let better = rechecked
                                        .as_ref()
                                        .map(|list| list.len() < review.issues.len())
                                        .unwrap_or(false)
                                        && words_ok;
                                    if better {
                                        let before = review.issues.len();
                                        let previous_issues = review.issues.clone();
                                        content = fixed;
                                        // 状态化合并：模糊匹配同名意见（不产生重复条目），
                                        // 不再出现的旧意见标记 fixed，多出来的标记「新发现」
                                        let mut latest = rechecked.unwrap_or_default();
                                        mark_quote_flags(&content, &mut latest);
                                        review.issues =
                                            merge_issue_statuses(&previous_issues, &latest);
                                        let open_left =
                                            review.issues.iter().filter(|i| i.status != "fixed").count();
                                        review.ok = open_left == 0;
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
                                        engine.db.save_chapter_content_sourced(
                                            novel_id, idx, &content, "drafted", "autofix",
                                        )?;
                                        engine.hub.publish(novel_id, EngineEvent::ChapterSaved {
                                            chapter_idx: idx,
                                            title: title.to_string(),
                                            words: content.chars().count(),
                                        });
                                        engine.hub.publish(novel_id, EngineEvent::Stage {
                                            stage: "autofix".into(),
                                            message: format!(
                                                "第 {idx} 章自动修复完成（{before} → {open_left} 条仍存在，{} 条已修复）",
                                                review.issues.iter().filter(|i| i.status == "fixed").count()
                                            ),
                                        });
                                    } else {
                                        engine.hub.publish(novel_id, EngineEvent::Stage {
                                            stage: "autofix".into(),
                                            message: if !words_ok {
                                                format!(
                                                    "第 {idx} 章自动修复稿篇幅异常（疑似截断），保留原稿（仍存在 {} 条意见）",
                                                    review.issues.iter().filter(|i| i.status != "fixed").count()
                                                )
                                            } else {
                                                format!(
                                                    "第 {idx} 章自动修复未比原稿更好，保留原稿（仍存在 {} 条意见，可打开章节逐条处理或点「重新审校」）",
                                                    review.issues.iter().filter(|i| i.status != "fixed").count()
                                                )
                                            },
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
                    // 记录正文指纹：正文之后被改动（手动编辑/单章操作）即可被巡检识别为漏审
                    review.content_hash = content_hash(&content);
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

    let review = parsed.ok_or_else(|| anyhow::anyhow!("审校输出无法解析（缺 issues 数组）"))?;
    *content_ref = content;
    Ok(review)
}

/// 章节是否还有待处理的审校（无审校记录，或存在仍为 open 的高/中危意见）
fn review_needs_processing(ch: &crate::models::Chapter) -> bool {
    match &ch.review_json {
        None => true,
        Some(raw) => serde_json::from_str::<Review>(raw)
            .map(|r| medium_high_count(&r) > 0)
            .unwrap_or(true),
    }
}

/// 仍存在（未标 fixed）的高/中危意见数。
/// 本地文风检查项不算：它由生成期重写与人工处理，「AI 处理全部审校」的队列只针对 LLM 审校意见，
/// 否则每轮都会把同一章重新入队、永不收敛。
pub fn medium_high_count(review: &Review) -> usize {
    review
        .issues
        .iter()
        .filter(|i| !is_style_issue(i))
        .filter(|i| i.status != "fixed")
        .filter(|i| i.severity == "high" || i.severity == "medium")
        .count()
}

fn is_style_issue(i: &ReviewIssue) -> bool {
    i.note.starts_with("文风检查：")
}

/// 汇总审校意见状态（旧 → 新）：
/// - 旧意见在「新列表」里找到同义条目（模糊匹配：文案高相似 / 互相包含 / 引用互含）→ open（采用新表述，不产生重复条目）
/// - 旧意见不再出现 → fixed（保留旧文案，供追溯「已修复了什么」）
/// - 新列表中多出来的意见 → open 且标记 is_new（「新发现」）
/// - 旧列表里已是 fixed 的条目继续保留
pub fn merge_issue_statuses(previous: &[ReviewIssue], latest: &[ReviewIssue]) -> Vec<ReviewIssue> {
    let mut out: Vec<ReviewIssue> = Vec::new();
    let mut matched: Vec<bool> = vec![false; latest.len()];
    for p in previous {
        if p.status == "fixed" {
            out.push(p.clone());
            continue;
        }
        let hit = latest
            .iter()
            .enumerate()
            .find(|(i, l)| !matched[*i] && issues_match(p, l));
        match hit {
            Some((i, l)) => {
                matched[i] = true;
                out.push(ReviewIssue {
                    status: "open".into(),
                    is_new: false,
                    ..l.clone()
                });
            }
            None => out.push(ReviewIssue {
                status: "fixed".into(),
                ..p.clone()
            }),
        }
    }
    for (i, l) in latest.iter().enumerate() {
        if !matched[i] {
            out.push(ReviewIssue {
                status: "open".into(),
                is_new: true,
                ..l.clone()
            });
        }
    }
    out
}

/// 两条意见是否指向同一问题：文案相似 / 互相包含 / 引用互相包含
fn issues_match(prev: &ReviewIssue, latest: &ReviewIssue) -> bool {
    if notes_similar(&prev.note, &latest.note) {
        return true;
    }
    let (pq, lq) = (prev.quote.trim(), latest.quote.trim());
    if !pq.is_empty() && !lq.is_empty() {
        if pq == lq {
            return true;
        }
        let (long, short) = if pq.chars().count() >= lq.chars().count() {
            (pq, lq)
        } else {
            (lq, pq)
        };
        if short.chars().count() >= 6 && long.contains(short) {
            return true;
        }
    }
    false
}

/// 意见文案相似：完全相同 / 一方包含另一方（≥8 字） / 字符二元组 Jaccard ≥ 0.6（≥6 字）
fn notes_similar(a: &str, b: &str) -> bool {
    let (a, b) = (a.trim(), b.trim());
    if a.is_empty() || b.is_empty() {
        return false;
    }
    if a == b {
        return true;
    }
    let (long, short) = if a.chars().count() >= b.chars().count() {
        (a, b)
    } else {
        (b, a)
    };
    if short.chars().count() >= 8 && long.contains(short) {
        return true;
    }
    if a.chars().count() < 6 || b.chars().count() < 6 {
        return false;
    }
    goal_similarity(a, b) >= 0.6
}

/// 校验并标记意见引用的可定位性（stale = 引用在当前正文中找不到）
pub fn mark_quote_flags(content: &str, issues: &mut [ReviewIssue]) {
    for i in issues.iter_mut() {
        if i.status == "fixed" {
            i.stale = false;
            continue;
        }
        i.stale = !crate::models::quote_located(content, &i.quote);
    }
}

/// 伏笔自动审核：用章节摘要批量判断哪些未回收伏笔已被后文实质回收，自动标记。
/// 每次批量运行收尾与「一键 AI 处理全部审校」时调用，返回新回收条数。
async fn audit_foreshadowings(engine: &Arc<Engine>, novel_id: &str) -> anyhow::Result<usize> {
    let open: Vec<crate::models::Foreshadowing> = engine
        .db
        .list_foreshadowings(novel_id)?
        .into_iter()
        .filter(|f| f.status == "open")
        .collect();
    if open.is_empty() {
        return Ok(0);
    }
    let chapters = engine.db.list_chapter_summaries(novel_id)?;
    let summaries: Vec<String> = chapters
        .iter()
        .filter(|(_, _, summary)| !summary.trim().is_empty())
        .map(|(idx, title, summary)| format!("第{}章 {}：{}", idx, title, summary.trim()))
        .collect();
    if summaries.is_empty() {
        return Ok(0);
    }
    engine.hub.publish(novel_id, EngineEvent::Stage {
        stage: "extract".into(),
        message: format!("伏笔自动审核：核对 {} 条未回收伏笔…", open.len()),
    });
    let settings = engine.llm_settings()?;
    let llm = Llm::new(settings.clone());
    let list = open
        .iter()
        .map(|f| format!("- {}（第{}章埋下）", f.title, f.planted_chapter))
        .collect::<Vec<_>>()
        .join("\n");
    // 长书的回收证据通常出现在最近的章节：截断取「最近 6000 字」而不是最早 6000 字
    let sum_text: String = tail_chars(&summaries.join("\n"), 6000);
    let ov = engine.prompt_overrides();
    let (sys, usr) = crate::prompts::foreshadow_audit_prompt(&list, &sum_text, &ov);
    match llm.complete(&sys, &usr).await {
        Ok(out) => {
            record_usage(
                &engine.db,
                Some(novel_id),
                None,
                "foreshadow_audit",
                &settings,
                &out.usage,
            );
            let resolved = apply_foreshadow_audit(&engine.db, &open, &out.text);
            if resolved > 0 {
                engine.hub.publish(novel_id, EngineEvent::LedgerUpdated {
                    chapter_idx: 0,
                    characters: 0,
                    foreshadowings: resolved,
                });
                engine.hub.publish(novel_id, EngineEvent::Stage {
                    stage: "extract".into(),
                    message: format!("伏笔自动审核：回收 {resolved} 条"),
                });
            }
            Ok(resolved)
        }
        Err(e) => {
            tracing::warn!("foreshadow audit failed: {e:#}");
            Ok(0)
        }
    }
}

/// 应用伏笔审核结果（纯逻辑，便于单测）：按标题精确匹配未回收清单后标记回收
pub fn apply_foreshadow_audit(
    db: &crate::db::Db,
    open: &[crate::models::Foreshadowing],
    raw: &str,
) -> usize {
    let Some(v) = extract_json_object(raw) else {
        return 0;
    };
    let Some(arr) = v["resolved"].as_array() else {
        return 0;
    };
    let mut n = 0usize;
    for item in arr {
        let Some(title) = item["title"].as_str() else {
            continue;
        };
        let Some(f) = open.iter().find(|f| f.title.trim() == title.trim()) else {
            continue;
        };
        let chapter_idx = item["chapter_idx"].as_i64().unwrap_or(0);
        let evidence = item["evidence"].as_str().unwrap_or("").trim();
        let notes = if f.notes.trim().is_empty() {
            if evidence.is_empty() {
                "AI 自动审核确认已回收".to_string()
            } else {
                format!("AI 自动审核：{evidence}")
            }
        } else {
            f.notes.clone()
        };
        if db
            .update_foreshadowing(&f.novel_id, &f.id, "resolved", &notes, Some(chapter_idx))
            .unwrap_or(false)
        {
            n += 1;
        }
    }
    n
}

/// 审校名额守卫：无论函数从哪条路径返回、任务如何结束（含 panic），Drop 都会释放
struct ReviewSlot {
    engine: Arc<Engine>,
    novel_id: String,
}

impl Drop for ReviewSlot {
    fn drop(&mut self) {
        self.engine.end_review(&self.novel_id);
    }
}

/// 「AI 处理全部审校」：对有内容的、存在待处理问题的章节重新审校并自动修复。
/// 返回待处理章节数（0 = 都已处理）；后台执行，进度经 SSE 推送。
/// 同一本书同时只允许一轮（Engine 里有名额）；处理期间可被「停止」中断，且与创作运行互斥。
pub fn spawn_review_all(engine: Arc<Engine>, novel_id: &str) -> anyhow::Result<i64> {
    if !engine.begin_review(novel_id) {
        anyhow::bail!("已有一轮审校处理在进行中");
    }
    let slot = ReviewSlot {
        engine: engine.clone(),
        novel_id: novel_id.to_string(),
    };

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
        // 章节审校都已处理：若仍有未回收伏笔，也跑一轮伏笔自动审核（不返回队列数）
        let has_open_foreshadow = engine
            .db
            .list_foreshadowings(novel_id)?
            .iter()
            .any(|f| f.status == "open");
        if !has_open_foreshadow {
            // G3：没有待处理章节时直接释放名额，并检查自动连载续批
            drop(slot);
            let _ = maybe_chain_auto(&engine, novel_id);
            return Ok(0);
        }
    }
    let total = targets.len() as i64;
    let style_id = novel.style_preset.clone();
    let nid = novel_id.to_string();
    tokio::spawn(async move {
        let _slot = slot; // 任务结束（含中途 return）才释放
        let style = crate::styles::find(&style_id);
        let ov = engine.prompt_overrides();
        let mut digested = 0usize;
        for (i, (idx, title, goal)) in targets.iter().enumerate() {
            if engine.stop_requested(&nid) {
                engine.clear_stop(&nid);
                engine.hub.publish(&nid, EngineEvent::Stage {
                    stage: "done".into(),
                    message: "审校处理已停止（未处理的章节保持原样）".into(),
                });
                return;
            }
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
                    // 保持定稿状态：内部采纳修复稿时会以 drafted 落库，这里恢复 done，
                    // 否则下一轮「继续创作」会按章纲整章重写、覆盖刚修好的稿
                    if ch.status == "done" {
                        let _ = engine.db.save_chapter_content_sourced(&nid, *idx, &content, "done", "review-all");
                    }
                    // AI 复原检查：若处理过程把综合分改差（问题变多/篇幅塌陷），自动恢复更优快照
                    let _ = ai_restore_chapter(&engine, &nid, *idx, "ai-restore").await;
                }
                Err(e) => tracing::warn!("review-all chapter {idx} failed: {e:#}"),
            }
        }
        // 阶段二前：补写缺失卷摘要（审校可能补齐了缺失章节的摘要）
        let last_done = engine
            .db
            .list_chapter_metas(&nid)
            .map(|cs| {
                cs.iter()
                    .filter(|c| c.status == "done")
                    .map(|c| c.idx)
                    .max()
                    .unwrap_or(0)
            })
            .unwrap_or(0);
        let _ = maybe_fill_volume_summaries(&engine, &nid, last_done).await;
        let resolved = audit_foreshadowings(&engine, &nid).await.unwrap_or(0);
        // 阶段三：全局审校（跨章一致性/节奏/支线；含自动定向修复与复检）
        let (gr, gfixed) = match global_review_and_fix(&engine, &nid, 3).await {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!("global review in review-all failed: {e:#}");
                (GlobalReview { ok: true, issues: Vec::new(), updated_at: String::new() }, 0)
            }
        };
        let gopen = gr.issues.iter().filter(|i| i.status != "fixed").count();
        let message = if total > 0 {
            format!(
                "AI 审校处理完成：复核 {total} 章，消化 {digested} 条问题；全局审校 {} 条（仍存在 {gopen}，自动修复 {gfixed} 章）；伏笔自动审核回收 {resolved} 条",
                gr.issues.len()
            )
        } else if !gr.issues.is_empty() {
            format!(
                "章节审校均已处理；全局审校 {} 条（仍存在 {gopen}，自动修复 {gfixed} 章）；伏笔自动审核回收 {resolved} 条",
                gr.issues.len()
            )
        } else {
            format!("章节审校均已处理；伏笔自动审核回收 {resolved} 条")
        };
        engine.hub.publish(&nid, EngineEvent::Stage {
            stage: "done".into(),
            message,
        });
        // G3：审校收尾后释放名额，若自动连载仍未达标则自动续批
        drop(_slot);
        let _ = maybe_chain_auto(&engine, &nid);
    });
    Ok(total)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::AppConfig;

    fn issue(severity: &str, note: &str, status: &str) -> ReviewIssue {
        ReviewIssue {
            severity: severity.into(),
            note: note.into(),
            quote: String::new(),
            status: status.into(),
            is_new: false,
            stale: false,
        }
    }

    fn tmp_dir(tag: &str) -> std::path::PathBuf {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("ainovel-pipeline-{tag}-{stamp}"))
    }

    #[test]
    fn merge_issue_statuses_marks_fixed_and_new_open() {
        let previous = vec![
            issue("high", "主角动机缺失", "open"),
            issue("medium", "时间线冲突", "open"),
        ];
        // 复核后：时间线冲突仍存在（同文案），新增一条低危；主角动机缺失不再出现
        let latest = vec![
            issue("medium", "时间线冲突", "open"),
            issue("low", "口头禅重复", "open"),
        ];
        let merged = merge_issue_statuses(&previous, &latest);
        assert_eq!(merged.len(), 3);
        let find = |note: &str| merged.iter().find(|i| i.note == note).cloned().unwrap();
        assert_eq!(find("时间线冲突").status, "open");
        assert_eq!(find("口头禅重复").status, "open");
        assert_eq!(find("主角动机缺失").status, "fixed");
    }

    #[test]
    fn merge_issue_statuses_keeps_previously_fixed_and_fixes_disappeared() {
        let previous = vec![
            issue("high", "旧问题A", "fixed"),
            issue("medium", "旧问题B", "open"),
        ];
        // 复核后没有任何意见：B 也应转为 fixed，A 保留
        let merged = merge_issue_statuses(&previous, &[]);
        assert_eq!(merged.len(), 2);
        assert!(merged.iter().all(|i| i.status == "fixed"));
        assert!(merged.iter().any(|i| i.note == "旧问题A"));
        assert!(merged.iter().any(|i| i.note == "旧问题B"));
    }

    #[test]
    fn merge_matches_rephrased_issue_without_duplicates() {
        // 复核用不同措辞描述同一个问题：应归并成一条 open，不应出现「已修复 + 新发现」双条
        let previous = vec![issue(
            "medium",
            "第3章与第7章的时间线冲突：主角同日出现在两地",
            "open",
        )];
        let latest = vec![issue(
            "medium",
            "时间线冲突：主角在第3章与第7章同日出现在两地",
            "open",
        )];
        let merged = merge_issue_statuses(&previous, &latest);
        assert_eq!(merged.len(), 1, "同义意见应归并：{merged:?}");
        assert_eq!(merged[0].status, "open");
        assert!(!merged[0].is_new, "归并命中的不是新发现");
        // 完全新增的意见 → open + is_new
        let latest2 = vec![issue(
            "low",
            "反复使用“不由得”这个词，口头禅重复",
            "open",
        )];
        let merged2 = merge_issue_statuses(&previous, &latest2);
        assert_eq!(merged2.len(), 2, "{merged2:?}");
        assert!(merged2.iter().any(|i| i.note.contains("不由得") && i.is_new));
        assert!(merged2.iter().any(|i| i.note.contains("时间线") && i.status == "fixed"));
    }

    #[test]
    fn quote_located_and_mark_flags() {
        use crate::models::quote_located;
        assert!(quote_located("他把钥匙插进锁孔。", "把钥匙插进锁孔"));
        assert!(quote_located("他把钥匙 插进锁孔。", "把钥匙插进锁孔"), "忽略空白差异");
        // 长引用被改写尾部：前缀能定位
        assert!(quote_located(
            "他把青铜钥匙插进锁孔，轻轻一转。",
            "他把青铜钥匙插进锁孔，用力一转"
        ));
        assert!(!quote_located("他把钥匙插进锁孔。", "窗外的雨停了"));

        let mut issues = vec![
            issue("medium", "节奏问题", "open"),
            issue("low", "已修复的旧条", "fixed"),
        ];
        issues[0].quote = "不存在的引文".into();
        issues[1].quote = "同样不存在的引文".into();
        mark_quote_flags("他把钥匙插进锁孔。", &mut issues);
        assert!(issues[0].stale, "引用对不上正文应标记 stale");
        assert!(!issues[1].stale, "已修复条目不参与 stale 标记");
        let mut ok_issue = vec![issue("low", "可定位", "open")];
        ok_issue[0].quote = "把钥匙".into();
        mark_quote_flags("他把钥匙插进锁孔。", &mut ok_issue);
        assert!(!ok_issue[0].stale);
    }

    #[test]
    fn stop_flags_roundtrip() {
        let dir = tmp_dir("stop");
        let engine = Engine::new(AppConfig::for_data_dir(dir.clone())).unwrap();
        let id = "novel-x";
        assert!(!engine.stop_requested(id));
        engine.request_stop(id);
        assert!(engine.stop_requested(id));
        engine.clear_stop(id);
        assert!(!engine.stop_requested(id));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn medium_high_count_ignores_fixed() {
        let review = Review {
            ok: false,
            issues: vec![
                issue("high", "a", "fixed"),
                issue("medium", "b", "open"),
                issue("low", "c", "open"),
            ],
            content_hash: String::new(),
        };
        assert_eq!(medium_high_count(&review), 1);
    }

    #[test]
    fn foreshadow_audit_applies_only_confident_resolutions() {
        let dir = tmp_dir("audit");
        let engine = Engine::new(AppConfig::for_data_dir(dir.clone())).unwrap();
        let novel = engine.db.create_novel("伏笔审核测试", "auto", "").unwrap();
        engine
            .db
            .add_foreshadowing(&novel.id, "青铜钥匙的来历", "")
            .unwrap();
        engine
            .db
            .add_foreshadowing(&novel.id, "守夜人的身份", "")
            .unwrap();
        let open = engine.db.list_foreshadowings(&novel.id).unwrap();
        assert_eq!(open.len(), 2);

        // 只回收标题精确匹配的；编造标题不生效
        let raw = r#"{"resolved":[{"title":"青铜钥匙的来历","chapter_idx":3,"evidence":"第3章已交代来历"},{"title":"不存在的伏笔"}]}"#;
        let n = apply_foreshadow_audit(&engine.db, &open, raw);
        assert_eq!(n, 1, "只有精确匹配的伏笔被回收");
        let after = engine.db.list_foreshadowings(&novel.id).unwrap();
        let key = after.iter().find(|f| f.title == "青铜钥匙的来历").unwrap();
        assert_eq!(key.status, "resolved");
        assert_eq!(key.resolved_chapter, Some(3));
        assert!(key.notes.contains("已交代来历"));
        let other = after.iter().find(|f| f.title == "守夜人的身份").unwrap();
        assert_eq!(other.status, "open");

        // 非 JSON / 缺字段：静默返回 0
        assert_eq!(apply_foreshadow_audit(&engine.db, &open, "not json"), 0);
        assert_eq!(apply_foreshadow_audit(&engine.db, &open, r#"{"resolved":[]}"#), 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn preflight_flags_dead_character_and_duplicate_goal() {
        let dir = tmp_dir("preflight");
        let engine = Engine::new(AppConfig::for_data_dir(dir.clone())).unwrap();
        let novel = engine.db.create_novel("预检测试", "auto", "").unwrap();
        engine
            .db
            .upsert_character(&novel.id, "老陈", "配角", "dead", &[])
            .unwrap();
        for i in 1..=2i64 {
            engine
                .db
                .upsert_chapter_plan(
                    &novel.id,
                    &crate::models::ChapterPlan {
                        idx: i,
                        title: format!("第{i}章"),
                        goal: "主角在码头与老陈对峙".into(),
                        beats: vec!["开场".into()],
                    },
                )
                .unwrap();
        }
        let plan = crate::models::ChapterPlan {
            idx: 3,
            title: "第3章".into(),
            goal: "主角在码头与老陈对峙".into(),
            beats: vec!["对峙升级".into()],
        };
        let warnings = preflight_warnings(&engine.db, &novel.id, 3, &plan, "", 0);
        assert!(
            warnings.iter().any(|w| w.contains("老陈")),
            "应提示已死亡角色：{warnings:?}"
        );
        assert!(
            warnings.iter().any(|w| w.contains("高度相似")),
            "应提示与近章目标重复：{warnings:?}"
        );
        // 无矛盾时返回空
        let clean = crate::models::ChapterPlan {
            idx: 3,
            title: "第3章".into(),
            goal: "主角独自登船前往雾港".into(),
            beats: vec!["启程".into()],
        };
        let none = preflight_warnings(&engine.db, &novel.id, 3, &clean, "", 0);
        assert!(none.is_empty(), "{none:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn preflight_flags_flat_pace_and_repeated_hook() {
        let dir = tmp_dir("preflight-pacing");
        let engine = Engine::new(AppConfig::for_data_dir(dir.clone())).unwrap();
        let novel = engine.db.create_novel("节奏预检", "auto", "").unwrap();
        for i in 1..=4i64 {
            engine
                .db
                .upsert_chapter_plan(
                    &novel.id,
                    &crate::models::ChapterPlan {
                        idx: i,
                        title: format!("第{i}章"),
                        goal: String::new(),
                        beats: vec![],
                    },
                )
                .unwrap();
        }
        // 第 1、2 章为过渡强度（1、2）→ 第 3 章也低强度时应提示节奏偏平
        engine.db.set_chapter_pacing(&novel.id, 1, "悬念", 1).unwrap();
        engine.db.set_chapter_pacing(&novel.id, 2, "危机", 2).unwrap();
        let plan3 = crate::models::ChapterPlan {
            idx: 3,
            title: "第3章".into(),
            goal: String::new(),
            beats: vec![],
        };
        let warnings = preflight_warnings(&engine.db, &novel.id, 3, &plan3, "信息", 1);
        assert!(
            warnings.iter().any(|w| w.contains("节奏偏平")),
            "应提示连续过渡章：{warnings:?}"
        );

        // 第 3、4 章同为「反转」强钩（≥4）→ 第 4 章应提示钩型重复
        engine.db.set_chapter_pacing(&novel.id, 3, "反转", 4).unwrap();
        let plan4 = crate::models::ChapterPlan {
            idx: 4,
            title: "第4章".into(),
            goal: String::new(),
            beats: vec![],
        };
        let w2 = preflight_warnings(&engine.db, &novel.id, 4, &plan4, "反转", 4);
        assert!(
            w2.iter().any(|w| w.contains("钩型重复")),
            "应提示同型强钩连发：{w2:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn retry_backoff_schedule_and_month_boundary() {
        // 退避表：1/5/15 分钟，最多 3 次
        assert_eq!(retry_delay_secs(0), None);
        assert_eq!(retry_delay_secs(1), Some(60));
        assert_eq!(retry_delay_secs(2), Some(300));
        assert_eq!(retry_delay_secs(3), Some(900));
        assert_eq!(retry_delay_secs(4), None);
        // 跨月起点：12 月 → 次年 1 月 1 日 00:05
        use chrono::{Datelike, TimeZone, Timelike};
        let dec = chrono::Local.with_ymd_and_hms(2026, 12, 15, 10, 0, 0).single().unwrap();
        let next = next_month_start_local(dec);
        assert_eq!((next.year(), next.month(), next.day()), (2027, 1, 1));
        assert_eq!((next.hour(), next.minute()), (0, 5));
        let mar = chrono::Local.with_ymd_and_hms(2026, 3, 1, 0, 0, 0).single().unwrap();
        let next2 = next_month_start_local(mar);
        assert_eq!((next2.year(), next2.month()), (2026, 4));
    }

    #[test]
    fn run_batch_end_advances_by_batch_from_done_progress() {
        let dir = tmp_dir("batch-end");
        let engine = Engine::new(AppConfig::for_data_dir(dir.clone())).unwrap();
        let novel = engine.db.create_novel("批量终点", "auto", "").unwrap();
        engine.db.set_novel_target(&novel.id, 100).unwrap();
        for i in 1..=50i64 {
            engine
                .db
                .upsert_chapter_plan(
                    &novel.id,
                    &crate::models::ChapterPlan {
                        idx: i,
                        title: format!("第{i}章"),
                        goal: String::new(),
                        beats: vec![],
                    },
                )
                .unwrap();
            engine.db.save_chapter_content(&novel.id, i, "正文", "done").unwrap();
        }
        // 已写完 50 章、每批 10 → 「继续创作」写到第 60 章（不再被 50 封顶）
        assert_eq!(run_batch_end(&engine.db, &novel.id, 10).unwrap(), 60);
        // 目标更近时按目标封顶
        engine.db.set_novel_target(&novel.id, 55).unwrap();
        assert_eq!(run_batch_end(&engine.db, &novel.id, 10).unwrap(), 55);
        // 未设目标：按批大小前进
        engine.db.set_novel_target(&novel.id, 0).unwrap();
        assert_eq!(run_batch_end(&engine.db, &novel.id, 10).unwrap(), 60);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn volume_summary_generates_and_injects_into_context() {
        let dir = tmp_dir("volume");
        let engine = std::sync::Arc::new(Engine::new(AppConfig::for_data_dir(dir.clone())).unwrap());
        engine
            .db
            .set_llm_settings(&crate::models::LlmSettings::default())
            .unwrap();
        let novel = engine.db.create_novel("卷摘要测试", "auto", "").unwrap();
        for i in 1..=20i64 {
            engine
                .db
                .upsert_chapter_plan(
                    &novel.id,
                    &crate::models::ChapterPlan {
                        idx: i,
                        title: format!("第{i}章"),
                        goal: String::new(),
                        beats: vec![],
                    },
                )
                .unwrap();
            engine
                .db
                .save_chapter_content(&novel.id, i, "正文内容", "done")
                .unwrap();
            engine
                .db
                .set_chapter_summary(&novel.id, i, &format!("第{i}章摘要：某事发生"))
                .unwrap();
        }
        generate_volume_summary(&engine, &novel.id, 1).await.unwrap();
        let vols = engine.db.list_volume_summaries(&novel.id).unwrap();
        assert_eq!(vols.len(), 1, "应生成一条卷摘要");
        assert_eq!(vols[0].volume_idx, 1);
        assert_eq!(vols[0].upto_chapter, 20);
        assert!(!vols[0].summary.trim().is_empty());
        // 注入检查：第 25 章（下一卷）应看到卷摘要
        let ctx = recent_summaries_context(&engine.db, &novel.id, 25).unwrap();
        assert!(ctx.contains("前情回顾（卷摘要）"), "{ctx}");
        // 用量任务记录
        let groups = engine.db.usage_by_task(&novel.id).unwrap();
        assert!(
            groups.iter().any(|g| g.key == "volume_summary"),
            "应记录卷摘要用量：{:?}",
            groups.iter().map(|g| g.key.clone()).collect::<Vec<_>>()
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn volume_summary_heals_manually_written_boundary() {
        // 第 20 章由人工/单章接口完成（绕过流水线边界触发）时，补写入口仍能生成卷摘要
        let dir = tmp_dir("volume-heal");
        let engine = std::sync::Arc::new(Engine::new(AppConfig::for_data_dir(dir.clone())).unwrap());
        engine
            .db
            .set_llm_settings(&crate::models::LlmSettings::default())
            .unwrap();
        let novel = engine.db.create_novel("卷摘要补写测试", "auto", "").unwrap();
        for i in 1..=20i64 {
            engine
                .db
                .upsert_chapter_plan(
                    &novel.id,
                    &crate::models::ChapterPlan {
                        idx: i,
                        title: format!("第{i}章"),
                        goal: String::new(),
                        beats: vec![],
                    },
                )
                .unwrap();
            // 注意：不走 generate_volume_summary 的边界触发，模拟手工写完全部章节
            engine
                .db
                .save_chapter_content(&novel.id, i, "正文内容", "done")
                .unwrap();
            engine
                .db
                .set_chapter_summary(&novel.id, i, &format!("第{i}章摘要：某事发生"))
                .unwrap();
        }
        assert!(engine.db.list_volume_summaries(&novel.id).unwrap().is_empty());
        let made = maybe_fill_volume_summaries(&engine, &novel.id, 20).await.unwrap();
        assert!(made >= 1, "应补写出第 1 卷摘要");
        let vols = engine.db.list_volume_summaries(&novel.id).unwrap();
        assert_eq!(vols.len(), 1);
        assert_eq!(vols[0].volume_idx, 1);
        // 幂等：再跑一次不重复生成
        let again = maybe_fill_volume_summaries(&engine, &novel.id, 20).await.unwrap();
        assert_eq!(again, 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn merge_global_issues_reuses_status_lifecycle() {
        use crate::models::GlobalIssue;
        let gi = |note: &str, status: &str| GlobalIssue {
            severity: "medium".into(),
            note: note.into(),
            affected_chapters: vec![2],
            suggestion: String::new(),
            status: status.into(),
            is_new: false,
        };
        let previous = vec![
            gi("连续三章缺乏冲突升级", "open"),
            gi("主角目标模糊，读者不知道他想干什么", "open"),
        ];
        let latest = vec![
            gi("连续三章缺乏冲突升级（第 5~7 章）", "open"),
            gi("第 9 章信息过载，塞入了太多新设定", "open"),
        ];
        let merged = merge_global_issues(&previous, &latest);
        assert_eq!(merged.len(), 3);
        let find = |n: &str| merged.iter().find(|i| i.note.contains(n)).unwrap();
        assert_eq!(find("连续三章").status, "open");
        assert!(!find("连续三章").is_new, "换措辞的描述应归并为同一条");
        assert!(find("连续三章").note.contains("第 5~7 章"), "采用新表述");
        assert_eq!(find("主角目标模糊").status, "fixed");
        assert_eq!(find("信息过载").status, "open");
        assert!(find("信息过载").is_new);
    }

    #[tokio::test]
    async fn ai_restore_picks_better_version() {
        let dir = tmp_dir("airestore");
        let engine = std::sync::Arc::new(Engine::new(AppConfig::for_data_dir(dir.clone())).unwrap());
        engine
            .db
            .set_llm_settings(&crate::models::LlmSettings::default())
            .unwrap();
        let novel = engine.db.create_novel("AI 复原测试", "auto", "").unwrap();
        engine
            .db
            .upsert_chapter_plan(
                &novel.id,
                &crate::models::ChapterPlan {
                    idx: 1,
                    title: "第一章".into(),
                    goal: String::new(),
                    beats: vec![],
                },
            )
            .unwrap();
        // 第一版：篇幅正常、带 1 条 high open
        engine
            .db
            .save_chapter_content(
                &novel.id,
                1,
                "第一版正文，内容较长，至少要有足够的字数用于评分比较。",
                "done",
            )
            .unwrap();
        let review_one = crate::models::Review {
            ok: false,
            issues: vec![ReviewIssue {
                severity: "high".into(),
                note: "开场冲突不足".into(),
                quote: String::new(),
                status: "open".into(),
                is_new: false,
                stale: false,
            }],
            content_hash: String::new(),
        };
        engine.db.save_chapter_review(&novel.id, 1, &review_one).unwrap();
        // 覆盖为更差版本：极短 + 3 条 open（旧版自动留档，带其审校快照）
        engine
            .db
            .save_chapter_content_sourced(&novel.id, 1, "第二版，太短。", "done", "manual")
            .unwrap();
        let review_three = crate::models::Review {
            ok: false,
            issues: (0..3)
                .map(|i| ReviewIssue {
                    severity: "medium".into(),
                    note: format!("问题{i}"),
                    quote: String::new(),
                    status: "open".into(),
                    is_new: false,
                    stale: false,
                })
                .collect(),
            content_hash: String::new(),
        };
        engine.db.save_chapter_review(&novel.id, 1, &review_three).unwrap();

        // AI 复原：旧版（1 问题 + 篇幅正常）综合分更高 → 应恢复
        let changed = ai_restore_chapter(&engine, &novel.id, 1, "ai-restore").await.unwrap();
        assert!(changed.is_some(), "应发生一次复原");
        let ch = engine.db.get_chapter(&novel.id, 1).unwrap().unwrap();
        assert!(ch.content.contains("第一版正文"), "{}", ch.content);
        assert!(
            ch.review_json.as_deref().unwrap_or("").contains("开场冲突不足"),
            "复原的审校应与其正文是一致快照"
        );
        // 已是综合最优：再跑一次不应再改
        let again = ai_restore_chapter(&engine, &novel.id, 1, "ai-restore").await.unwrap();
        assert!(again.is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn stale_review_candidates_track_content_changes() {
        let dir = tmp_dir("stale-cand");
        let engine = Engine::new(AppConfig::for_data_dir(dir.clone())).unwrap();
        let novel = engine.db.create_novel("漏审候选测试", "auto", "").unwrap();
        engine
            .db
            .upsert_chapter_plan(
                &novel.id,
                &crate::models::ChapterPlan {
                    idx: 1,
                    title: "第一章".into(),
                    goal: String::new(),
                    beats: vec![],
                },
            )
            .unwrap();
        engine.db.save_chapter_content(&novel.id, 1, "第一版正文内容。", "done").unwrap();
        // 旧数据：无指纹 → 不入选（避免升级后全量重审）
        engine
            .db
            .save_chapter_review(
                &novel.id,
                1,
                &Review {
                    ok: false,
                    issues: vec![issue("medium", "问题A", "open")],
                    content_hash: String::new(),
                },
            )
            .unwrap();
        assert!(stale_review_candidates(&engine.db, &novel.id, 10).is_empty());
        // 带指纹但内容未变 → 不入选
        engine
            .db
            .save_chapter_review(
                &novel.id,
                1,
                &Review {
                    ok: false,
                    issues: vec![issue("medium", "问题A", "open")],
                    content_hash: content_hash("第一版正文内容。"),
                },
            )
            .unwrap();
        assert!(stale_review_candidates(&engine.db, &novel.id, 10).is_empty());
        // 手动改动正文 → 入选（这正是「漏审」场景）
        engine
            .db
            .save_chapter_edit(&novel.id, 1, Some("第二版正文，被手动改过了。"), None)
            .unwrap();
        let c = stale_review_candidates(&engine.db, &novel.id, 10);
        assert_eq!(c.len(), 1);
        assert_eq!(c[0].0, 1);
        // 没有未解决的高/中危意见 → 不入选
        engine
            .db
            .save_chapter_review(
                &novel.id,
                1,
                &Review { ok: true, issues: vec![], content_hash: String::new() },
            )
            .unwrap();
        assert!(stale_review_candidates(&engine.db, &novel.id, 10).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn maintenance_fixes_state_and_fills_summaries() {
        let dir = tmp_dir("maintain");
        let engine = std::sync::Arc::new(Engine::new(AppConfig::for_data_dir(dir.clone())).unwrap());
        engine
            .db
            .set_llm_settings(&crate::models::LlmSettings::default())
            .unwrap();
        let novel = engine.db.create_novel("巡检测试", "auto", "").unwrap();
        // 章1：有正文但无摘要 → 应补
        engine
            .db
            .upsert_chapter_plan(
                &novel.id,
                &crate::models::ChapterPlan {
                    idx: 1,
                    title: "第一章".into(),
                    goal: String::new(),
                    beats: vec![],
                },
            )
            .unwrap();
        engine
            .db
            .save_chapter_content(&novel.id, 1, "第一章正文，用于巡检补摘要。", "done")
            .unwrap();
        // 章2：done 但无正文（UI 会误显示已完成）→ 状态应收回 planned
        engine
            .db
            .upsert_chapter_plan(
                &novel.id,
                &crate::models::ChapterPlan {
                    idx: 2,
                    title: "第二章".into(),
                    goal: String::new(),
                    beats: vec![],
                },
            )
            .unwrap();
        engine.db.save_chapter_content(&novel.id, 2, "", "done").unwrap();

        let r = maintain_novel(&engine, &novel.id, 5, 0).await.unwrap();
        assert_eq!(r.fixed_empty_status, 1);
        assert_eq!(r.filled_summaries, 1);
        let ch1 = engine.db.get_chapter(&novel.id, 1).unwrap().unwrap();
        assert!(!ch1.summary.trim().is_empty(), "缺摘要应被补齐");
        let ch2 = engine.db.get_chapter(&novel.id, 2).unwrap().unwrap();
        assert_eq!(ch2.status, "planned", "done 无正文应被收回 planned");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
