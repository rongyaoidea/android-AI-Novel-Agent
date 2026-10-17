use std::sync::Mutex;

use anyhow::Context;
use rusqlite::{params, Connection, OptionalExtension, Row};

use crate::models::*;

/// 单连接 + Mutex 的 SQLite 封装。
/// 本地单用户场景下简单可靠；后续如需并发可换连接池。
pub struct Db {
    conn: Mutex<Connection>,
}

pub const LLM_SETTINGS_KEY: &str = "llm";
pub const PROMPT_OVERRIDES_KEY: &str = "prompt_overrides";
pub const PIPELINE_SETTINGS_KEY: &str = "pipeline";

fn now() -> String {
    chrono::Utc::now().to_rfc3339()
}

const NOVEL_COLS: &str = "id,title,genre,idea,logline,protagonist,style,book_outline,status,mode,created_at,updated_at,style_preset,target_chapters,auto_continue,worldview,clarify_json";

/// 每章保留的历史版本数
const MAX_CHAPTER_VERSIONS: i64 = 10;

/// CJK 判定（含扩展 A、兼容表意文字与日文假名；用于 FTS 逐字拆分）
fn is_cjk(c: char) -> bool {
    matches!(
        c as u32,
        0x3400..=0x4DBF | 0x4E00..=0x9FFF | 0xF900..=0xFAFF | 0x3040..=0x30FF
    )
}

/// 检索摘录：首个命中词的上下文（前后各 30 / 60 字符）；未命中取开头 80 字
fn excerpt_for(content: &str, query: &str) -> String {
    let needle = query.split_whitespace().next().unwrap_or("").trim();
    if !needle.is_empty() {
        if let Some(pos) = content.find(needle) {
            let before: String = {
                let mut v: Vec<char> = content[..pos].chars().rev().take(30).collect();
                v.reverse();
                v.into_iter().collect()
            };
            let width = needle.chars().count() + 60;
            let after: String = content[pos..].chars().take(width).collect();
            return format!("…{before}{after}…");
        }
    }
    let head: String = content.chars().take(80).collect();
    format!("{head}…")
}
const CHAPTER_COLS: &str = "id,novel_id,idx,title,goal,beats_json,content,summary,status,review_json,created_at,updated_at,style_report,hook_type,intensity";
const RUN_COLS: &str = "id,novel_id,status,stage,chapter_cursor,chapters_target,error,created_at,updated_at";

impl Db {
    pub fn open(path: &std::path::Path) -> anyhow::Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let conn = Connection::open(path)
            .with_context(|| format!("open sqlite at {}", path.display()))?;
        conn.execute_batch(
            "PRAGMA journal_mode=WAL;
             PRAGMA foreign_keys=ON;
             PRAGMA busy_timeout=5000;",
        )?;
        let db = Self { conn: Mutex::new(conn) };
        db.migrate()?;
        db.recover_stale_runs()?;
        Ok(db)
    }

    /// 启动自愈：把上次进程中断遗留的 running 运行标记为失败。
    /// 设计前提是单进程独占数据目录（Android 壳 / 桌面 dev 均如此）。
    fn recover_stale_runs(&self) -> anyhow::Result<usize> {
        let conn = self.conn.lock().unwrap();
        let n = conn.execute(
            "UPDATE runs SET status='failed', error='进程中断（启动时自动标记，可重新发起创作）', updated_at=?1
             WHERE status='running'",
            [now()],
        )?;
        if n > 0 {
            tracing::warn!("启动自愈：{n} 条中断的运行已标记为 failed");
        }
        Ok(n)
    }

    fn migrate(&self) -> anyhow::Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS settings (
                key   TEXT PRIMARY KEY,
                value TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS novels (
                id           TEXT PRIMARY KEY,
                title        TEXT NOT NULL DEFAULT '',
                genre        TEXT NOT NULL DEFAULT '',
                idea         TEXT NOT NULL,
                logline      TEXT NOT NULL DEFAULT '',
                protagonist  TEXT NOT NULL DEFAULT '',
                style        TEXT NOT NULL DEFAULT '',
                book_outline TEXT NOT NULL DEFAULT '',
                status       TEXT NOT NULL DEFAULT 'created',
                mode         TEXT NOT NULL DEFAULT 'auto',
                created_at   TEXT NOT NULL,
                updated_at   TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS chapters (
                id          TEXT PRIMARY KEY,
                novel_id    TEXT NOT NULL REFERENCES novels(id) ON DELETE CASCADE,
                idx         INTEGER NOT NULL,
                title       TEXT NOT NULL DEFAULT '',
                goal        TEXT NOT NULL DEFAULT '',
                beats_json  TEXT NOT NULL DEFAULT '[]',
                content     TEXT NOT NULL DEFAULT '',
                summary     TEXT NOT NULL DEFAULT '',
                status      TEXT NOT NULL DEFAULT 'planned',
                review_json TEXT,
                created_at  TEXT NOT NULL,
                updated_at  TEXT NOT NULL,
                UNIQUE(novel_id, idx)
            );
            CREATE TABLE IF NOT EXISTS runs (
                id              TEXT PRIMARY KEY,
                novel_id        TEXT NOT NULL REFERENCES novels(id) ON DELETE CASCADE,
                status          TEXT NOT NULL,
                stage           TEXT NOT NULL,
                chapter_cursor  INTEGER NOT NULL DEFAULT 0,
                chapters_target INTEGER NOT NULL,
                error           TEXT,
                created_at      TEXT NOT NULL,
                updated_at      TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS characters (
                id         TEXT PRIMARY KEY,
                novel_id   TEXT NOT NULL REFERENCES novels(id) ON DELETE CASCADE,
                name       TEXT NOT NULL,
                role       TEXT NOT NULL DEFAULT '',
                status     TEXT NOT NULL DEFAULT 'active',
                facts      TEXT NOT NULL DEFAULT '[]',
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL,
                UNIQUE(novel_id, name)
            );
            CREATE TABLE IF NOT EXISTS foreshadowings (
                id               TEXT PRIMARY KEY,
                novel_id         TEXT NOT NULL REFERENCES novels(id) ON DELETE CASCADE,
                title            TEXT NOT NULL,
                status           TEXT NOT NULL DEFAULT 'open',
                planted_chapter  INTEGER NOT NULL DEFAULT 0,
                resolved_chapter INTEGER,
                notes            TEXT NOT NULL DEFAULT '',
                created_at       TEXT NOT NULL,
                updated_at       TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_chapters_novel ON chapters(novel_id, idx);
            CREATE INDEX IF NOT EXISTS idx_runs_novel ON runs(novel_id, created_at);
            CREATE INDEX IF NOT EXISTS idx_characters_novel ON characters(novel_id, updated_at);
            CREATE INDEX IF NOT EXISTS idx_foreshadowings_novel ON foreshadowings(novel_id, status);
            CREATE TABLE IF NOT EXISTS llm_usage (
                id                TEXT PRIMARY KEY,
                novel_id          TEXT,
                chapter_idx       INTEGER,
                task              TEXT NOT NULL,
                provider          TEXT NOT NULL,
                model             TEXT NOT NULL,
                input_tokens      INTEGER,
                output_tokens     INTEGER,
                cache_hit_tokens  INTEGER,
                cache_miss_tokens INTEGER,
                total_tokens      INTEGER,
                created_at        TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_usage_novel ON llm_usage(novel_id, created_at);
            CREATE INDEX IF NOT EXISTS idx_usage_task ON llm_usage(novel_id, task);
            "#,
        )?;
        // 旧库升级：补 novels.mode 列
        Self::ensure_column(&conn, "novels", "mode", "mode TEXT NOT NULL DEFAULT 'auto'")?;
        Self::ensure_column(
            &conn,
            "novels",
            "style_preset",
            "style_preset TEXT NOT NULL DEFAULT ''",
        )?;
        Self::ensure_column(
            &conn,
            "chapters",
            "style_report",
            "style_report TEXT NOT NULL DEFAULT ''",
        )?;
        Self::ensure_column(
            &conn,
            "novels",
            "target_chapters",
            "target_chapters INTEGER NOT NULL DEFAULT 0",
        )?;
        Self::ensure_column(
            &conn,
            "novels",
            "auto_continue",
            "auto_continue INTEGER NOT NULL DEFAULT 0",
        )?;
        // 旧库升级：补 chapters.summary 列
        Self::ensure_column(&conn, "chapters", "summary", "summary TEXT NOT NULL DEFAULT ''")?;
        // 旧库升级：补 novels.worldview 列（立项世界观）
        Self::ensure_column(&conn, "novels", "worldview", "worldview TEXT NOT NULL DEFAULT ''")?;
        // 旧库升级：补 novels.clarify_json 列（立项澄清问答）
        Self::ensure_column(&conn, "novels", "clarify_json", "clarify_json TEXT NOT NULL DEFAULT ''")?;
        // 旧库升级：补 chapters.hook_type / intensity 列（章纲节奏字段：章末钩型 + 推进强度）
        Self::ensure_column(&conn, "chapters", "hook_type", "hook_type TEXT NOT NULL DEFAULT ''")?;
        Self::ensure_column(&conn, "chapters", "intensity", "intensity INTEGER NOT NULL DEFAULT 0")?;
        // 卷级摘要（每 20 章归并一条，供长书回顾注入）
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS volume_summaries (
                novel_id     TEXT NOT NULL,
                volume_idx   INTEGER NOT NULL,
                upto_chapter INTEGER NOT NULL,
                summary      TEXT NOT NULL,
                created_at   TEXT NOT NULL,
                updated_at   TEXT NOT NULL,
                PRIMARY KEY(novel_id, volume_idx)
             );
             CREATE TABLE IF NOT EXISTS volume_reports (
                novel_id     TEXT NOT NULL,
                volume_idx   INTEGER NOT NULL,
                upto_chapter INTEGER NOT NULL,
                report_json  TEXT NOT NULL,
                created_at   TEXT NOT NULL,
                updated_at   TEXT NOT NULL,
                PRIMARY KEY(novel_id, volume_idx)
             );
             CREATE TABLE IF NOT EXISTS book_reports (
                novel_id    TEXT NOT NULL,
                kind        TEXT NOT NULL,
                report_json TEXT NOT NULL,
                created_at  TEXT NOT NULL,
                updated_at  TEXT NOT NULL,
                PRIMARY KEY(novel_id, kind)
             );
             CREATE TABLE IF NOT EXISTS chapter_versions (
                id           TEXT PRIMARY KEY,
                novel_id     TEXT NOT NULL,
                idx          INTEGER NOT NULL,
                source       TEXT NOT NULL,
                status       TEXT NOT NULL DEFAULT 'done',
                content      TEXT NOT NULL,
                review_json  TEXT,
                style_report TEXT NOT NULL DEFAULT '',
                created_at   TEXT NOT NULL
             );
             CREATE INDEX IF NOT EXISTS idx_chapter_versions ON chapter_versions(novel_id, idx, created_at);
             CREATE TABLE IF NOT EXISTS global_reviews (
                novel_id    TEXT PRIMARY KEY,
                review_json TEXT NOT NULL,
                updated_at  TEXT NOT NULL
             );",
        )?;
        // 全文检索（FTS5，CJK 逐字空格化后入库；查询用同规则归一化）
        conn.execute_batch(
            "CREATE VIRTUAL TABLE IF NOT EXISTS chapter_fts USING fts5(novel_id UNINDEXED, chapter_idx UNINDEXED, title, body);",
        )?;
        // ① 负区占位还原（必须早于 FTS 自愈，避免把负章号写进索引）。
        //    极少数「旧版本重排写到一半崩溃」的库可能还原出冲突章号：逐行改用空闲位置，保证能启动。
        let fixed = Self::recover_negative_indexes(&conn)?;
        if fixed > 0 {
            tracing::warn!("启动自愈：{fixed} 章的重排残留（负章号）已还原");
        }
        // ② FTS 自愈：与章节数不一致（首次启用 / 旧备份 / 崩溃残留 / 标题章缺失）的书重建索引
        let mismatched: Vec<String> = {
            let mut stmt = conn.prepare(
                "SELECT n.id FROM novels n
                 WHERE (SELECT COUNT(*) FROM chapters c WHERE c.novel_id = n.id)
                    != (SELECT COUNT(*) FROM chapter_fts f WHERE f.novel_id = n.id)",
            )?;
            let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
            let mut v = Vec::new();
            for r in rows {
                v.push(r?);
            }
            v
        };
        let mut healed = 0usize;
        for nid in &mismatched {
            Self::reindex_novel_locked(&conn, nid)?;
            healed += 1;
        }
        if healed > 0 {
            tracing::info!("全文索引已自愈 {healed} 本（回填/重建）");
        }
        Ok(())
    }

    /// 把负区占位章号（重排中断残留）还原；与既有章号冲突时放到该书末尾空闲位置。
    /// 还原后重建受影响作品的检索索引（章号变了，索引必须跟着走）。
    fn recover_negative_indexes(conn: &Connection) -> anyhow::Result<usize> {
        let mut stmt = conn.prepare(
            "SELECT novel_id, id, idx FROM chapters WHERE idx < 0 ORDER BY novel_id ASC, idx DESC",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, i64>(2)?,
            ))
        })?;
        let mut items: Vec<(String, String, i64)> = Vec::new();
        for r in rows {
            items.push(r?);
        }
        drop(stmt);
        let mut touched: std::collections::HashSet<String> = std::collections::HashSet::new();
        let mut fixed = 0usize;
        for (nid, id, neg) in items {
            let want = -neg - 1;
            let free = want >= 1
                && conn
                    .query_row(
                        "SELECT 1 FROM chapters WHERE novel_id=?1 AND idx=?2",
                        params![nid, want],
                        |_| Ok(true),
                    )
                    .optional()?
                    .is_none();
            let target = if free {
                want
            } else {
                // 冲突兜底：放到该书末尾（保持可启动；用户可再用「上移/下移」调整）
                conn.query_row(
                    "SELECT COALESCE(MAX(idx), 0) + 1 FROM chapters WHERE novel_id=?1",
                    params![nid],
                    |r| r.get::<_, i64>(0),
                )?
                .max(1)
            };
            conn.execute(
                "UPDATE chapters SET idx=?2 WHERE id=?1",
                params![id, target],
            )?;
            touched.insert(nid);
            fixed += 1;
        }
        for nid in &touched {
            Self::reindex_novel_locked(conn, nid)?;
        }
        Ok(fixed)
    }

    /// FTS 归一化：CJK 逐字拆成独立 token（空格分隔），拉丁转小写，其余（标点/空白）作分隔符。
    /// 索引与查询用同一规则，多字词用引号短语匹配，即近似「包含」检索（中文无需分词器）。
    pub fn fts_normalize(s: &str) -> String {
        let mut out = String::with_capacity(s.len() + s.len() / 2);
        for c in s.chars() {
            if is_cjk(c) {
                if !out.is_empty() && !out.ends_with(' ') {
                    out.push(' ');
                }
                out.push(c);
                out.push(' ');
            } else if c.is_alphanumeric() {
                for lc in c.to_lowercase() {
                    out.push(lc);
                }
            } else if !out.is_empty() && !out.ends_with(' ') {
                out.push(' ');
            }
        }
        out.split_whitespace().collect::<Vec<_>>().join(" ")
    }

    /// 生成 FTS5 MATCH 表达式：每个 token 用引号短语，空格连接（AND）
    fn fts_query(q: &str) -> String {
        Self::fts_normalize(q)
            .split_whitespace()
            .map(|t| format!("\"{t}\""))
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// 重建一章的全文索引（无内容也写标题，便于按章名检索）
    fn reindex_chapter_locked(conn: &Connection, novel_id: &str, idx: i64) -> rusqlite::Result<()> {
        conn.execute(
            "DELETE FROM chapter_fts WHERE novel_id=?1 AND chapter_idx=?2",
            params![novel_id, idx],
        )?;
        let row = conn.query_row(
            "SELECT title, content FROM chapters WHERE novel_id=?1 AND idx=?2",
            params![novel_id, idx],
            |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
        );
        match row {
            Ok((title, content)) => {
                conn.execute(
                    "INSERT INTO chapter_fts(novel_id, chapter_idx, title, body) VALUES(?1,?2,?3,?4)",
                    params![novel_id, idx, Self::fts_normalize(&title), Self::fts_normalize(&content)],
                )?;
            }
            Err(rusqlite::Error::QueryReturnedNoRows) => {}
            Err(e) => return Err(e),
        }
        Ok(())
    }

    /// 重建一本书的全部全文索引（章节重排 / 结构编辑后调用）
    pub fn reindex_novel(&self, novel_id: &str) -> anyhow::Result<()> {
        let conn = self.conn.lock().unwrap();
        Self::reindex_novel_locked(&conn, novel_id)
    }

    fn reindex_novel_locked(conn: &Connection, novel_id: &str) -> anyhow::Result<()> {
        conn.execute("DELETE FROM chapter_fts WHERE novel_id=?1", [novel_id])?;
        let mut stmt = conn.prepare(
            "SELECT idx, title, content FROM chapters WHERE novel_id=?1 ORDER BY idx ASC",
        )?;
        let rows = stmt.query_map([novel_id], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
            ))
        })?;
        for row in rows {
            let (idx, title, content) = row?;
            conn.execute(
                "INSERT INTO chapter_fts(novel_id, chapter_idx, title, body) VALUES(?1,?2,?3,?4)",
                params![novel_id, idx, Self::fts_normalize(&title), Self::fts_normalize(&content)],
            )?;
        }
        Ok(())
    }

    /// 轻量迁移：列不存在时 ALTER TABLE 补齐（幂等）。
    fn ensure_column(conn: &Connection, table: &str, column: &str, ddl: &str) -> anyhow::Result<()> {
        let mut exists = false;
        {
            let mut stmt = conn.prepare(&format!("PRAGMA table_info({table})"))?;
            let mut rows = stmt.query([])?;
            while let Some(row) = rows.next()? {
                let name: String = row.get(1)?;
                if name == column {
                    exists = true;
                    break;
                }
            }
        }
        if !exists {
            conn.execute_batch(&format!("ALTER TABLE {table} ADD COLUMN {ddl}"))?;
        }
        Ok(())
    }

    // ---------- settings ----------

    pub fn get_setting(&self, key: &str) -> anyhow::Result<Option<String>> {
        let conn = self.conn.lock().unwrap();
        let v = conn
            .query_row("SELECT value FROM settings WHERE key=?1", [key], |r| r.get::<_, String>(0))
            .optional()?;
        Ok(v)
    }

    pub fn set_setting(&self, key: &str, value: &str) -> anyhow::Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO settings(key, value) VALUES(?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value=excluded.value",
            params![key, value],
        )?;
        Ok(())
    }

    pub fn llm_settings(&self) -> anyhow::Result<LlmSettings> {
        Ok(match self.get_setting(LLM_SETTINGS_KEY)? {
            Some(raw) => serde_json::from_str(&raw).unwrap_or_default(),
            None => LlmSettings::default(),
        })
    }

    pub fn set_llm_settings(&self, s: &LlmSettings) -> anyhow::Result<()> {
        self.set_setting(LLM_SETTINGS_KEY, &serde_json::to_string(s)?)
    }

    /// 提示词覆盖（JSON：任务 id → {system, user}）；空串表示全部恢复默认
    pub fn prompt_overrides_json(&self) -> anyhow::Result<Option<String>> {
        Ok(self
            .get_setting(PROMPT_OVERRIDES_KEY)?
            .filter(|s| !s.trim().is_empty() && s.trim() != "{}"))
    }

    pub fn set_prompt_overrides_json(&self, json: &str) -> anyhow::Result<()> {
        self.set_setting(PROMPT_OVERRIDES_KEY, json)
    }

    /// 写作流水线偏好（解析失败时回退默认值）
    pub fn pipeline_settings(&self) -> anyhow::Result<PipelineSettings> {
        Ok(match self.get_setting(PIPELINE_SETTINGS_KEY)? {
            Some(raw) => serde_json::from_str(&raw).unwrap_or_default(),
            None => PipelineSettings::default(),
        })
    }

    pub fn set_pipeline_settings(&self, s: &PipelineSettings) -> anyhow::Result<()> {
        self.set_setting(PIPELINE_SETTINGS_KEY, &serde_json::to_string(s)?)
    }

    // ---------- novels ----------

    pub fn create_novel(&self, idea: &str, mode: &str, style_preset: &str) -> anyhow::Result<Novel> {
        let n = Novel {
            id: uuid::Uuid::new_v4().to_string(),
            title: String::new(),
            genre: String::new(),
            idea: idea.to_string(),
            logline: String::new(),
            protagonist: String::new(),
            style: String::new(),
            book_outline: String::new(),
            worldview: String::new(),
            clarify_json: String::new(),
            status: "created".into(),
            mode: if mode == "staged" { "staged".into() } else { "auto".into() },
            created_at: now(),
            updated_at: now(),
            style_preset: style_preset.trim().to_string(),
            target_chapters: 0,
            auto_continue: false,
        };
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO novels(id,title,genre,idea,logline,protagonist,style,book_outline,status,mode,created_at,updated_at,style_preset)
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13)",
            params![n.id, n.title, n.genre, n.idea, n.logline, n.protagonist, n.style, n.book_outline, n.status, n.mode, n.created_at, n.updated_at, n.style_preset],
        )?;
        Ok(n)
    }

    fn map_novel(row: &Row<'_>) -> rusqlite::Result<Novel> {
        Ok(Novel {
            id: row.get(0)?,
            title: row.get(1)?,
            genre: row.get(2)?,
            idea: row.get(3)?,
            logline: row.get(4)?,
            protagonist: row.get(5)?,
            style: row.get(6)?,
            book_outline: row.get(7)?,
            status: row.get(8)?,
            mode: row.get(9)?,
            created_at: row.get(10)?,
            updated_at: row.get(11)?,
            style_preset: row.get(12)?,
            target_chapters: row.get(13)?,
            auto_continue: row.get(14)?,
            worldview: row.get(15)?,
            clarify_json: row.get(16)?,
        })
    }

    pub fn get_novel(&self, id: &str) -> anyhow::Result<Option<Novel>> {
        let conn = self.conn.lock().unwrap();
        let sql = format!("SELECT {NOVEL_COLS} FROM novels WHERE id=?1");
        Ok(conn.query_row(&sql, [id], Self::map_novel).optional()?)
    }

    /// 列表：附带已完成章节数、总字数与最近一次运行状态
    pub fn list_novels(&self) -> anyhow::Result<Vec<NovelListItem>> {
        let conn = self.conn.lock().unwrap();
        let sql = format!(
            "SELECT {NOVEL_COLS},
                    (SELECT COUNT(*) FROM chapters c WHERE c.novel_id = novels.id AND c.status = 'done'),
                    (SELECT COALESCE(SUM(LENGTH(c.content)), 0) FROM chapters c WHERE c.novel_id = novels.id),
                    (SELECT r.status FROM runs r WHERE r.novel_id = novels.id ORDER BY r.created_at DESC LIMIT 1),
                    (SELECT r.stage FROM runs r WHERE r.novel_id = novels.id ORDER BY r.created_at DESC LIMIT 1),
                    (SELECT r.chapters_target FROM runs r WHERE r.novel_id = novels.id ORDER BY r.created_at DESC LIMIT 1),
                    (SELECT r.error FROM runs r WHERE r.novel_id = novels.id ORDER BY r.created_at DESC LIMIT 1)
             FROM novels ORDER BY updated_at DESC"
        );
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map([], |row| {
            Ok(NovelListItem {
                novel: Self::map_novel(row)?,
                chapters_done: row.get(17)?,
                words: row.get(18)?,
                run_status: row.get(19)?,
                run_stage: row.get(20)?,
                chapters_target: row.get(21)?,
                last_error: row.get(22)?,
            })
        })?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    pub fn delete_novel(&self, id: &str) -> anyhow::Result<bool> {
        let conn = self.conn.lock().unwrap();
        // FTS 表无外键级联：手动清理该书的检索索引
        conn.execute("DELETE FROM chapter_fts WHERE novel_id=?1", [id])?;
        conn.execute("DELETE FROM volume_summaries WHERE novel_id=?1", [id])?;
        conn.execute("DELETE FROM volume_reports WHERE novel_id=?1", [id])?;
        conn.execute("DELETE FROM book_reports WHERE novel_id=?1", [id])?;
        conn.execute("DELETE FROM chapter_versions WHERE novel_id=?1", [id])?;
        conn.execute("DELETE FROM global_reviews WHERE novel_id=?1", [id])?;
        let n = conn.execute("DELETE FROM novels WHERE id=?1", [id])?;
        Ok(n > 0)
    }

    pub fn update_novel_brief(&self, id: &str, b: &Brief) -> anyhow::Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE novels SET title=?2, genre=?3, logline=?4, protagonist=?5, style=?6, worldview=?8, updated_at=?7 WHERE id=?1",
            params![id, b.title, b.genre, b.logline, b.protagonist, b.style, now(), b.worldview],
        )?;
        Ok(())
    }

    pub fn update_novel_outline(&self, id: &str, outline: &str, status: &str) -> anyhow::Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE novels SET book_outline=?2, status=?3, updated_at=?4 WHERE id=?1",
            params![id, outline, status, now()],
        )?;
        Ok(())
    }

    /// 保存立项澄清问答（JSON 数组字符串）
    pub fn set_novel_clarify(&self, id: &str, clarify_json: &str) -> anyhow::Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE novels SET clarify_json=?2, updated_at=?3 WHERE id=?1",
            params![id, clarify_json, now()],
        )?;
        Ok(())
    }

    pub fn set_novel_status(&self, id: &str, status: &str) -> anyhow::Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE novels SET status=?2, updated_at=?3 WHERE id=?1",
            params![id, status, now()],
        )?;
        Ok(())
    }

    // ---------- chapters ----------

    fn map_chapter(row: &Row<'_>) -> rusqlite::Result<Chapter> {
        // 列顺序: id(0) novel_id(1) idx(2) title(3) goal(4) beats_json(5)
        //          content(6) summary(7) status(8) review_json(9) created_at(10) updated_at(11)
        //          style_report(12) hook_type(13) intensity(14)
        let beats_json: String = row.get(5)?;
        Ok(Chapter {
            id: row.get(0)?,
            novel_id: row.get(1)?,
            idx: row.get(2)?,
            title: row.get(3)?,
            goal: row.get(4)?,
            beats: serde_json::from_str(&beats_json).unwrap_or_default(),
            hook_type: row.get(13)?,
            intensity: row.get(14)?,
            content: row.get(6)?,
            summary: row.get(7)?,
            status: row.get(8)?,
            review_json: row.get(9)?,
            created_at: row.get(10)?,
            updated_at: row.get(11)?,
            style_report: row.get(12)?,
        })
    }

    pub fn get_chapter(&self, novel_id: &str, idx: i64) -> anyhow::Result<Option<Chapter>> {
        let conn = self.conn.lock().unwrap();
        let sql = format!("SELECT {CHAPTER_COLS} FROM chapters WHERE novel_id=?1 AND idx=?2");
        Ok(conn.query_row(&sql, params![novel_id, idx], Self::map_chapter).optional()?)
    }

    pub fn list_chapters(&self, novel_id: &str) -> anyhow::Result<Vec<Chapter>> {
        let conn = self.conn.lock().unwrap();
        let sql = format!("SELECT {CHAPTER_COLS} FROM chapters WHERE novel_id=?1 ORDER BY idx ASC");
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map([novel_id], Self::map_chapter)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    pub fn upsert_chapter_plan(&self, novel_id: &str, plan: &ChapterPlan) -> anyhow::Result<()> {
        let beats_json = serde_json::to_string(&plan.beats)?;
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO chapters(id, novel_id, idx, title, goal, beats_json, content, status, created_at, updated_at)
             VALUES(?1, ?2, ?3, ?4, ?5, ?6, '', 'planned', ?7, ?7)
             ON CONFLICT(novel_id, idx) DO UPDATE SET
                title=excluded.title,
                goal=excluded.goal,
                beats_json=excluded.beats_json,
                updated_at=excluded.updated_at
             WHERE chapters.status = 'planned'",
            params![uuid::Uuid::new_v4().to_string(), novel_id, plan.idx, plan.title, plan.goal, beats_json, now()],
        )?;
        // 章名变化同步到检索索引（先删后插，幂等）
        Self::reindex_chapter_locked(&conn, novel_id, plan.idx)?;
        Ok(())
    }

    pub fn save_chapter_content(&self, novel_id: &str, idx: i64, content: &str, status: &str) -> anyhow::Result<()> {
        self.save_chapter_content_sourced(novel_id, idx, content, status, "auto")
    }

    /// 写正文（带来源标签）：覆盖非空旧稿前自动留一份历史快照（保留最近 10 版）
    pub fn save_chapter_content_sourced(
        &self,
        novel_id: &str,
        idx: i64,
        content: &str,
        status: &str,
        source: &str,
    ) -> anyhow::Result<()> {
        let conn = self.conn.lock().unwrap();
        Self::snapshot_chapter_version_locked(&conn, novel_id, idx, source, content)?;
        conn.execute(
            "UPDATE chapters SET content=?3, status=?4, updated_at=?5 WHERE novel_id=?1 AND idx=?2",
            params![novel_id, idx, content, status, now()],
        )?;
        Self::reindex_chapter_locked(&conn, novel_id, idx)?;
        Self::refresh_review_staleness_locked(&conn, novel_id, idx)?;
        Ok(())
    }

    /// 快照当前正文（供版本历史与 AI 复原）。规则：
    /// - 旧内容为空、或与新内容相同 → 不留档
    /// - 每章保留最近 MAX_CHAPTER_VERSIONS 版
    /// - source 用「pre-操作」形态（pre-autofix / pre-manual / …）说明这是「什么操作之前」的版本
    fn snapshot_chapter_version_locked(
        conn: &Connection,
        novel_id: &str,
        idx: i64,
        source: &str,
        new_content: &str,
    ) -> anyhow::Result<()> {
        let row = conn.query_row(
            "SELECT content, status, review_json, style_report FROM chapters WHERE novel_id=?1 AND idx=?2",
            params![novel_id, idx],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, Option<String>>(2)?,
                    r.get::<_, String>(3)?,
                ))
            },
        );
        let (old, status, review, style) = match row {
            Ok(v) => v,
            Err(rusqlite::Error::QueryReturnedNoRows) => return Ok(()),
            Err(e) => return Err(e.into()),
        };
        if old.trim().is_empty() || old == new_content {
            return Ok(());
        }
        conn.execute(
            "INSERT INTO chapter_versions(id, novel_id, idx, source, status, content, review_json, style_report, created_at)
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9)",
            params![
                uuid::Uuid::new_v4().to_string(),
                novel_id,
                idx,
                format!("pre-{source}"),
                status,
                old,
                review,
                style,
                now()
            ],
        )?;
        // 保留最近 N 版，其余清理
        conn.execute(
            "DELETE FROM chapter_versions WHERE novel_id=?1 AND idx=?2 AND id NOT IN (
                SELECT id FROM chapter_versions WHERE novel_id=?1 AND idx=?2
                ORDER BY created_at DESC, rowid DESC LIMIT ?3
             )",
            params![novel_id, idx, MAX_CHAPTER_VERSIONS],
        )?;
        Ok(())
    }

    /// 章节历史版本（新的在前）
    pub fn list_chapter_versions(
        &self,
        novel_id: &str,
        idx: i64,
    ) -> anyhow::Result<Vec<crate::models::ChapterVersion>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, novel_id, idx, source, status, content, review_json, style_report, created_at
             FROM chapter_versions WHERE novel_id=?1 AND idx=?2 ORDER BY created_at DESC, rowid DESC",
        )?;
        let rows = stmt.query_map(params![novel_id, idx], |r| {
            Ok(crate::models::ChapterVersion {
                id: r.get(0)?,
                novel_id: r.get(1)?,
                idx: r.get(2)?,
                source: r.get(3)?,
                status: r.get(4)?,
                content: r.get(5)?,
                review_json: r.get(6)?,
                style_report: r.get(7)?,
                created_at: r.get(8)?,
            })
        })?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    /// 单条历史版本（校验归属，防跨书读取）
    pub fn get_chapter_version(
        &self,
        novel_id: &str,
        idx: i64,
        version_id: &str,
    ) -> anyhow::Result<Option<crate::models::ChapterVersion>> {
        Ok(self
            .list_chapter_versions(novel_id, idx)?
            .into_iter()
            .find(|v| v.id == version_id))
    }

    /// 复原到指定历史版本：先给当前稿留快照，再恢复正文 + 当时的审校/文风报告
    pub fn restore_chapter_version(
        &self,
        novel_id: &str,
        idx: i64,
        version_id: &str,
        via: &str,
    ) -> anyhow::Result<bool> {
        let Some(v) = self.get_chapter_version(novel_id, idx, version_id)? else {
            return Ok(false);
        };
        let conn = self.conn.lock().unwrap();
        // 写正文（内部会先快照当前稿，标签 pre-<via>）
        Self::snapshot_chapter_version_locked(&conn, novel_id, idx, via, &v.content)?;
        conn.execute(
            "UPDATE chapters SET content=?3, status=?4, review_json=?5, style_report=?6, updated_at=?7
             WHERE novel_id=?1 AND idx=?2",
            params![
                novel_id,
                idx,
                v.content,
                if v.status.trim().is_empty() { "done" } else { v.status.as_str() },
                v.review_json,
                v.style_report,
                now()
            ],
        )?;
        Self::reindex_chapter_locked(&conn, novel_id, idx)?;
        // 复原的审校与其正文是一致快照，不需要 stale 刷新；但保持调用以保证结构一致
        Self::refresh_review_staleness_locked(&conn, novel_id, idx)?;
        Ok(true)
    }

    /// 全局审校结论
    pub fn get_global_review(&self, novel_id: &str) -> anyhow::Result<Option<crate::models::GlobalReview>> {
        let conn = self.conn.lock().unwrap();
        let raw: Option<String> = conn
            .query_row(
                "SELECT review_json FROM global_reviews WHERE novel_id=?1",
                [novel_id],
                |r| r.get(0),
            )
            .optional()?;
        Ok(raw.and_then(|s| serde_json::from_str::<crate::models::GlobalReview>(&s).ok()))
    }

    /// 保存全局审校结论
    pub fn save_global_review(
        &self,
        novel_id: &str,
        review: &crate::models::GlobalReview,
    ) -> anyhow::Result<()> {
        let conn = self.conn.lock().unwrap();
        let raw = serde_json::to_string(review)?;
        conn.execute(
            "INSERT INTO global_reviews(novel_id, review_json, updated_at) VALUES(?1,?2,?3)
             ON CONFLICT(novel_id) DO UPDATE SET review_json=excluded.review_json, updated_at=excluded.updated_at",
            params![novel_id, raw, now()],
        )?;
        Ok(())
    }

    /// 正文变化后刷新审校意见引用的有效性（stale = 引用在当前正文中定位不到）。
    /// 「已修复」条目是历史留档，不参与标记。
    fn refresh_review_staleness_locked(
        conn: &Connection,
        novel_id: &str,
        idx: i64,
    ) -> anyhow::Result<()> {
        let row = conn.query_row(
            "SELECT content, review_json FROM chapters WHERE novel_id=?1 AND idx=?2",
            params![novel_id, idx],
            |r| Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?)),
        );
        let (content, raw) = match row {
            Ok(v) => v,
            Err(rusqlite::Error::QueryReturnedNoRows) => return Ok(()),
            Err(e) => return Err(e.into()),
        };
        let Some(raw) = raw else { return Ok(()) };
        let Ok(mut review) = serde_json::from_str::<Review>(&raw) else {
            return Ok(());
        };
        let mut changed = false;
        for issue in review.issues.iter_mut() {
            let stale = if issue.status == "fixed" {
                false
            } else {
                !crate::models::quote_located(&content, &issue.quote)
            };
            if issue.stale != stale {
                issue.stale = stale;
                changed = true;
            }
        }
        if changed {
            conn.execute(
                "UPDATE chapters SET review_json=?3 WHERE novel_id=?1 AND idx=?2",
                params![novel_id, idx, serde_json::to_string(&review)?],
            )?;
        }
        Ok(())
    }

    pub fn save_chapter_review(&self, novel_id: &str, idx: i64, review: &Review) -> anyhow::Result<()> {
        let conn = self.conn.lock().unwrap();
        // fixed 留档有上限，防止 review_json 随多轮修复无限膨胀（open 不裁）
        let mut capped = review.clone();
        crate::models::cap_review_fixed(&mut capped, crate::models::MAX_FIXED_ISSUES);
        conn.execute(
            "UPDATE chapters SET review_json=?3, updated_at=?4 WHERE novel_id=?1 AND idx=?2",
            params![novel_id, idx, serde_json::to_string(&capped)?, now()],
        )?;
        Ok(())
    }

    /// 保存本地文风检查报告（JSON 字符串；空串表示未检查）
    pub fn save_chapter_style_report(
        &self,
        novel_id: &str,
        idx: i64,
        report_json: &str,
    ) -> anyhow::Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE chapters SET style_report=?3, updated_at=?4 WHERE novel_id=?1 AND idx=?2",
            params![novel_id, idx, report_json, now()],
        )?;
        Ok(())
    }

    pub fn set_chapter_summary(&self, novel_id: &str, idx: i64, summary: &str) -> anyhow::Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE chapters SET summary=?3, updated_at=?4 WHERE novel_id=?1 AND idx=?2",
            params![novel_id, idx, summary, now()],
        )?;
        Ok(())
    }

    /// 修正章节状态（巡检用：如 done 但无正文 → 收回 planned 让下一轮重写）
    pub fn set_chapter_status(&self, novel_id: &str, idx: i64, status: &str) -> anyhow::Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE chapters SET status=?3, updated_at=?4 WHERE novel_id=?1 AND idx=?2",
            params![novel_id, idx, status, now()],
        )?;
        Ok(())
    }

    /// 章节摘要列表（只取 idx/title/summary，不加载正文；供回顾注入 / 卷摘要 / 伏笔审核复用）
    pub fn list_chapter_summaries(
        &self,
        novel_id: &str,
    ) -> anyhow::Result<Vec<(i64, String, String)>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT idx, title, summary FROM chapters WHERE novel_id=?1 ORDER BY idx ASC",
        )?;
        let rows = stmt.query_map([novel_id], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
            ))
        })?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    /// 章节列表（轻量：不含正文；正文按需走章节详情）
    pub fn list_chapter_metas(&self, novel_id: &str) -> anyhow::Result<Vec<crate::models::ChapterMeta>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT idx, title, goal, beats_json, status, LENGTH(content),
                    SUBSTR(content, 1, 60), review_json, hook_type, intensity
             FROM chapters WHERE novel_id=?1 ORDER BY idx ASC",
        )?;
        let rows = stmt.query_map([novel_id], |r| {
            let beats_json: String = r.get(3)?;
            Ok(crate::models::ChapterMeta {
                idx: r.get(0)?,
                title: r.get(1)?,
                goal: r.get(2)?,
                beats: serde_json::from_str(&beats_json).unwrap_or_default(),
                hook_type: r.get(8)?,
                intensity: r.get(9)?,
                status: r.get(4)?,
                words: r.get(5)?,
                preview: r.get(6)?,
                review_json: r.get(7)?,
            })
        })?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    /// 保存 / 更新卷末体检报告（JSON）
    pub fn upsert_volume_report(
        &self,
        novel_id: &str,
        volume_idx: i64,
        upto_chapter: i64,
        report_json: &str,
    ) -> anyhow::Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO volume_reports(novel_id, volume_idx, upto_chapter, report_json, created_at, updated_at)
             VALUES(?1,?2,?3,?4,?5,?5)
             ON CONFLICT(novel_id, volume_idx) DO UPDATE SET
                upto_chapter=excluded.upto_chapter,
                report_json=excluded.report_json,
                updated_at=excluded.updated_at",
            params![novel_id, volume_idx, upto_chapter, report_json, now()],
        )?;
        Ok(())
    }

    /// 卷末体检报告列表（按卷号升序）：(卷号, 覆盖至章, report_json)
    pub fn list_volume_reports(&self, novel_id: &str) -> anyhow::Result<Vec<(i64, i64, String)>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT volume_idx, upto_chapter, report_json FROM volume_reports
             WHERE novel_id=?1 ORDER BY volume_idx ASC",
        )?;
        let rows = stmt.query_map([novel_id], |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?, r.get::<_, String>(2)?))
        })?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    /// 某卷是否已有体检报告
    pub fn has_volume_report(&self, novel_id: &str, volume_idx: i64) -> anyhow::Result<bool> {
        let conn = self.conn.lock().unwrap();
        let n: i64 = conn.query_row(
            "SELECT COUNT(*) FROM volume_reports WHERE novel_id=?1 AND volume_idx=?2",
            params![novel_id, volume_idx],
            |r| r.get(0),
        )?;
        Ok(n > 0)
    }

    /// 删除某卷体检报告（复检前强制重出时用）
    pub fn delete_volume_report(&self, novel_id: &str, volume_idx: i64) -> anyhow::Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "DELETE FROM volume_reports WHERE novel_id=?1 AND volume_idx=?2",
            params![novel_id, volume_idx],
        )?;
        Ok(())
    }

    /// 保存 / 更新全书报告（kind: "finish" 完本收尾与读者审查）
    pub fn upsert_book_report(
        &self,
        novel_id: &str,
        kind: &str,
        report_json: &str,
    ) -> anyhow::Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO book_reports(novel_id, kind, report_json, created_at, updated_at)
             VALUES(?1,?2,?3,?4,?4)
             ON CONFLICT(novel_id, kind) DO UPDATE SET
                report_json=excluded.report_json,
                updated_at=excluded.updated_at",
            params![novel_id, kind, report_json, now()],
        )?;
        Ok(())
    }

    /// 全书报告列表：(kind, report_json, updated_at)
    pub fn list_book_reports(&self, novel_id: &str) -> anyhow::Result<Vec<(String, String, String)>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT kind, report_json, updated_at FROM book_reports
             WHERE novel_id=?1 ORDER BY kind ASC",
        )?;
        let rows = stmt.query_map([novel_id], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?))
        })?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    /// 是否已有某类全书报告（完本收尾去重用）
    pub fn has_book_report(&self, novel_id: &str, kind: &str) -> anyhow::Result<bool> {
        let conn = self.conn.lock().unwrap();
        let n: i64 = conn.query_row(
            "SELECT COUNT(*) FROM book_reports WHERE novel_id=?1 AND kind=?2",
            params![novel_id, kind],
            |r| r.get(0),
        )?;
        Ok(n > 0)
    }

    /// 全文检索（FTS5）：返回 (章号, 章节名, 摘录)，按章号升序
    pub fn search_chapters(
        &self,
        novel_id: &str,
        query: &str,
        limit: i64,
    ) -> anyhow::Result<Vec<(i64, String, String)>> {
        let q = Self::fts_query(query);
        if q.trim().is_empty() {
            return Ok(Vec::new());
        }
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT chapter_fts.chapter_idx, c.title, c.content
             FROM chapter_fts
             JOIN chapters c ON c.novel_id = chapter_fts.novel_id AND c.idx = chapter_fts.chapter_idx
             WHERE chapter_fts.novel_id = ?1 AND chapter_fts MATCH ?2
             ORDER BY chapter_fts.chapter_idx ASC
             LIMIT ?3",
        )?;
        let rows = stmt.query_map(params![novel_id, q, limit], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (idx, title, content) = row?;
            out.push((idx, title, excerpt_for(&content, query)));
        }
        Ok(out)
    }

    /// 保存 / 更新卷级摘要
    pub fn upsert_volume_summary(
        &self,
        novel_id: &str,
        volume_idx: i64,
        upto_chapter: i64,
        summary: &str,
    ) -> anyhow::Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO volume_summaries(novel_id, volume_idx, upto_chapter, summary, created_at, updated_at)
             VALUES(?1,?2,?3,?4,?5,?5)
             ON CONFLICT(novel_id, volume_idx) DO UPDATE SET
                upto_chapter=excluded.upto_chapter,
                summary=excluded.summary,
                updated_at=excluded.updated_at",
            params![novel_id, volume_idx, upto_chapter, summary, now()],
        )?;
        Ok(())
    }

    /// 卷级摘要列表（按卷号升序）
    pub fn list_volume_summaries(
        &self,
        novel_id: &str,
    ) -> anyhow::Result<Vec<crate::models::VolumeSummary>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT volume_idx, upto_chapter, summary FROM volume_summaries
             WHERE novel_id=?1 ORDER BY volume_idx ASC",
        )?;
        let rows = stmt.query_map([novel_id], |r| {
            Ok(crate::models::VolumeSummary {
                volume_idx: r.get(0)?,
                upto_chapter: r.get(1)?,
                summary: r.get(2)?,
            })
        })?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    // ---------- runs ----------

    pub fn create_run(&self, novel_id: &str, chapters_target: i64) -> anyhow::Result<Run> {
        let r = Run {
            id: uuid::Uuid::new_v4().to_string(),
            novel_id: novel_id.to_string(),
            status: "running".into(),
            stage: "brief".into(),
            chapter_cursor: 0,
            chapters_target,
            error: None,
            created_at: now(),
            updated_at: now(),
        };
        let conn = self.conn.lock().unwrap();
        // 同一本书同时只允许一个运行（进程内单连接，检查+插入在同一把锁内，天然原子）
        let running: i64 = conn.query_row(
            "SELECT COUNT(*) FROM runs WHERE novel_id=?1 AND status='running'",
            [novel_id],
            |row| row.get(0),
        )?;
        if running > 0 {
            anyhow::bail!("已有创作运行进行中");
        }
        conn.execute(
            "INSERT INTO runs(id, novel_id, status, stage, chapter_cursor, chapters_target, error, created_at, updated_at)
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9)",
            params![r.id, r.novel_id, r.status, r.stage, r.chapter_cursor, r.chapters_target, r.error, r.created_at, r.updated_at],
        )?;
        Ok(r)
    }

    /// 是否存在进行中的运行（备份恢复前置检查用）；返回一本有运行的书
    pub fn any_running_run(&self) -> anyhow::Result<Option<String>> {
        let conn = self.conn.lock().unwrap();
        let v = conn
            .query_row(
                "SELECT novel_id FROM runs WHERE status='running' LIMIT 1",
                [],
                |r| r.get::<_, String>(0),
            )
            .optional()?;
        Ok(v)
    }

    fn map_run(row: &Row<'_>) -> rusqlite::Result<Run> {
        Ok(Run {
            id: row.get(0)?,
            novel_id: row.get(1)?,
            status: row.get(2)?,
            stage: row.get(3)?,
            chapter_cursor: row.get(4)?,
            chapters_target: row.get(5)?,
            error: row.get(6)?,
            created_at: row.get(7)?,
            updated_at: row.get(8)?,
        })
    }

    pub fn latest_run(&self, novel_id: &str) -> anyhow::Result<Option<Run>> {
        let conn = self.conn.lock().unwrap();
        let sql = format!("SELECT {RUN_COLS} FROM runs WHERE novel_id=?1 ORDER BY created_at DESC LIMIT 1");
        Ok(conn.query_row(&sql, [novel_id], Self::map_run).optional()?)
    }

    pub fn update_run(
        &self,
        run_id: &str,
        status: &str,
        stage: &str,
        cursor: i64,
        error: Option<&str>,
    ) -> anyhow::Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE runs SET status=?2, stage=?3, chapter_cursor=?4, error=?5, updated_at=?6 WHERE id=?1",
            params![run_id, status, stage, cursor, error, now()],
        )?;
        Ok(())
    }

    /// 运行记录（跨作品，最近优先）
    pub fn list_runs(&self, limit: i64) -> anyhow::Result<Vec<RunListItem>> {
        let conn = self.conn.lock().unwrap();
        let sql = format!(
            "SELECT {RUN_COLS},
                    COALESCE((SELECT n.title FROM novels n WHERE n.id = runs.novel_id), '')
             FROM runs ORDER BY created_at DESC LIMIT ?1"
        );
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map([limit], |row| {
            Ok(RunListItem {
                run: Self::map_run(row)?,
                novel_title: row.get(9)?,
            })
        })?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    // ---------- 作品设置 ----------

    pub fn set_novel_mode(&self, id: &str, mode: &str) -> anyhow::Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE novels SET mode=?2, updated_at=?3 WHERE id=?1",
            params![id, mode, now()],
        )?;
        Ok(())
    }

    /// 设置/清除文风预设（空串 = 不限）
    pub fn set_novel_style(&self, id: &str, style_preset: &str) -> anyhow::Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE novels SET style_preset=?2, updated_at=?3 WHERE id=?1",
            params![id, style_preset.trim(), now()],
        )?;
        Ok(())
    }

    /// 设置全书目标章节数（0 = 清除）
    pub fn set_novel_target(&self, id: &str, target: i64) -> anyhow::Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE novels SET target_chapters=?2, updated_at=?3 WHERE id=?1",
            params![id, target.max(0), now()],
        )?;
        Ok(())
    }

    /// 开启/停止自动连载
    pub fn set_novel_auto_continue(&self, id: &str, on: bool) -> anyhow::Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE novels SET auto_continue=?2, updated_at=?3 WHERE id=?1",
            params![id, if on { 1 } else { 0 }, now()],
        )?;
        Ok(())
    }

    /// 指定时间点之后的累计 token 用量（用于月度预算护栏；未知口径不计）
    pub fn usage_total_since(&self, since: &str) -> anyhow::Result<i64> {
        let conn = self.conn.lock().unwrap();
        let v: Option<i64> = conn.query_row(
            "SELECT SUM(total_tokens) FROM llm_usage WHERE created_at >= ?1",
            [since],
            |r| r.get(0),
        )?;
        Ok(v.unwrap_or(0))
    }

    // ---------- 章节：手动编辑 ----------

    /// 手动编辑：更新标题 / 正文；写了正文即视为完成（流水线不再覆盖）。
    pub fn save_chapter_edit(
        &self,
        novel_id: &str,
        idx: i64,
        content: Option<&str>,
        title: Option<&str>,
    ) -> anyhow::Result<()> {
        let conn = self.conn.lock().unwrap();
        if let Some(c) = content {
            // 手动编辑前留快照（预置版本历史，供回滚与 AI 复原）
            Self::snapshot_chapter_version_locked(&conn, novel_id, idx, "manual", c)?;
            conn.execute(
                "UPDATE chapters SET content=?3, status='done', updated_at=?4 WHERE novel_id=?1 AND idx=?2",
                params![novel_id, idx, c, now()],
            )?;
        }
        if let Some(t) = title {
            conn.execute(
                "UPDATE chapters SET title=?3, updated_at=?4 WHERE novel_id=?1 AND idx=?2",
                params![novel_id, idx, t, now()],
            )?;
        }
        Self::reindex_chapter_locked(&conn, novel_id, idx)?;
        if content.is_some() {
            // 手动编辑正文后：刷新审校意见引用有效性
            Self::refresh_review_staleness_locked(&conn, novel_id, idx)?;
        }
        Ok(())
    }

    /// 编辑规划（仅限尚未开写的章节）
    /// 保存章纲的节奏字段（章末钩型 / 推进强度；仅作用于尚未开写的章节）
    pub fn set_chapter_pacing(
        &self,
        novel_id: &str,
        idx: i64,
        hook_type: &str,
        intensity: i64,
    ) -> anyhow::Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE chapters SET hook_type=?3, intensity=?4, updated_at=?5
             WHERE novel_id=?1 AND idx=?2 AND status='planned'",
            params![novel_id, idx, hook_type.trim(), intensity.clamp(0, 5), now()],
        )?;
        Ok(())
    }

    pub fn update_chapter_plan(
        &self,
        novel_id: &str,
        idx: i64,
        title: &str,
        goal: &str,
        beats: &[String],
    ) -> anyhow::Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE chapters SET title=?3, goal=?4, beats_json=?5, updated_at=?6
             WHERE novel_id=?1 AND idx=?2 AND status='planned'",
            params![novel_id, idx, title, goal, serde_json::to_string(beats)?, now()],
        )?;
        Self::reindex_chapter_locked(&conn, novel_id, idx)?;
        Ok(())
    }

    // ---------- 角色台账 ----------

    fn map_character(row: &Row<'_>) -> rusqlite::Result<Character> {
        let facts_json: String = row.get(5)?;
        Ok(Character {
            id: row.get(0)?,
            novel_id: row.get(1)?,
            name: row.get(2)?,
            role: row.get(3)?,
            status: row.get(4)?,
            facts: serde_json::from_str(&facts_json).unwrap_or_default(),
            created_at: row.get(6)?,
            updated_at: row.get(7)?,
        })
    }

    pub fn list_characters(&self, novel_id: &str) -> anyhow::Result<Vec<Character>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id,novel_id,name,role,status,facts,created_at,updated_at
             FROM characters WHERE novel_id=?1 ORDER BY updated_at DESC",
        )?;
        let rows = stmt.query_map([novel_id], Self::map_character)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    pub fn add_character(&self, novel_id: &str, name: &str, role: &str) -> anyhow::Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO characters(id, novel_id, name, role, status, facts, created_at, updated_at)
             VALUES(?1,?2,?3,?4,'active','[]',?5,?5)
             ON CONFLICT(novel_id, name) DO UPDATE SET role=excluded.role, updated_at=excluded.updated_at",
            params![uuid::Uuid::new_v4().to_string(), novel_id, name, role, now()],
        )?;
        Ok(())
    }

    pub fn update_character(
        &self,
        novel_id: &str,
        id: &str,
        role: &str,
        status: &str,
        facts: &[String],
    ) -> anyhow::Result<bool> {
        let conn = self.conn.lock().unwrap();
        let n = conn.execute(
            "UPDATE characters SET role=?2, status=?3, facts=?4, updated_at=?5 WHERE id=?1 AND novel_id=?6",
            params![id, role, status, serde_json::to_string(facts)?, now(), novel_id],
        )?;
        Ok(n > 0)
    }

    pub fn delete_character(&self, novel_id: &str, id: &str) -> anyhow::Result<bool> {
        let conn = self.conn.lock().unwrap();
        Ok(conn.execute(
            "DELETE FROM characters WHERE id=?1 AND novel_id=?2",
            params![id, novel_id],
        )? > 0)
    }

    /// 写后抽取：合并角色事实（去重、保留最近 12 条）
    pub fn upsert_character(
        &self,
        novel_id: &str,
        name: &str,
        role: &str,
        status: &str,
        new_facts: &[String],
    ) -> anyhow::Result<()> {
        let conn = self.conn.lock().unwrap();
        let existing: Option<(String, String, String, String)> = conn
            .query_row(
                "SELECT id, role, status, facts FROM characters WHERE novel_id=?1 AND name=?2",
                params![novel_id, name],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .optional()?;
        let ts = now();
        match existing {
            Some((id, old_role, old_status, facts_json)) => {
                let mut facts: Vec<String> = serde_json::from_str(&facts_json).unwrap_or_default();
                for f in new_facts {
                    if !facts.iter().any(|x| x == f) {
                        facts.push(f.clone());
                    }
                }
                if facts.len() > 12 {
                    let drop_n = facts.len() - 12;
                    facts.drain(0..drop_n);
                }
                let role = if role.trim().is_empty() { old_role } else { role.to_string() };
                let status = if status.trim().is_empty() { old_status } else { status.to_string() };
                conn.execute(
                    "UPDATE characters SET role=?2, status=?3, facts=?4, updated_at=?5 WHERE id=?1",
                    params![id, role, status, serde_json::to_string(&facts)?, ts],
                )?;
            }
            None => {
                conn.execute(
                    "INSERT INTO characters(id, novel_id, name, role, status, facts, created_at, updated_at)
                     VALUES(?1,?2,?3,?4,?5,?6,?7,?7)",
                    params![
                        uuid::Uuid::new_v4().to_string(),
                        novel_id,
                        name,
                        role,
                        if status.trim().is_empty() { "active" } else { status },
                        serde_json::to_string(new_facts)?,
                        ts
                    ],
                )?;
            }
        }
        Ok(())
    }

    /// 写后抽取：从 LLM JSON 吸收角色
    pub fn absorb_characters(&self, novel_id: &str, j: &serde_json::Value) -> anyhow::Result<usize> {
        let Some(arr) = j["characters"].as_array() else {
            return Ok(0);
        };
        let mut n = 0;
        for item in arr {
            let name = item["name"].as_str().unwrap_or("").trim();
            if name.is_empty() {
                continue;
            }
            let role = item["role"].as_str().unwrap_or("").trim();
            // 缺失 status 时用空串：upsert 会保留台账里的人工状态（如手工标记的 dead），
            // 新角色则由 upsert 的插入分支兜底为 active
            let status = item["status"].as_str().unwrap_or("").trim();
            let facts: Vec<String> = item["facts"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|x| x.as_str())
                        .map(|s| s.trim().to_string())
                        .filter(|s| !s.is_empty())
                        .collect()
                })
                .unwrap_or_default();
            self.upsert_character(novel_id, name, role, status, &facts)?;
            n += 1;
        }
        Ok(n)
    }

    // ---------- 伏笔台账 ----------

    fn map_foreshadowing(row: &Row<'_>) -> rusqlite::Result<Foreshadowing> {
        Ok(Foreshadowing {
            id: row.get(0)?,
            novel_id: row.get(1)?,
            title: row.get(2)?,
            status: row.get(3)?,
            planted_chapter: row.get(4)?,
            resolved_chapter: row.get(5)?,
            notes: row.get(6)?,
            created_at: row.get(7)?,
            updated_at: row.get(8)?,
        })
    }

    pub fn list_foreshadowings(&self, novel_id: &str) -> anyhow::Result<Vec<Foreshadowing>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id,novel_id,title,status,planted_chapter,resolved_chapter,notes,created_at,updated_at
             FROM foreshadowings WHERE novel_id=?1
             ORDER BY CASE status WHEN 'open' THEN 0 ELSE 1 END, planted_chapter DESC, updated_at DESC",
        )?;
        let rows = stmt.query_map([novel_id], Self::map_foreshadowing)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    pub fn add_foreshadowing(&self, novel_id: &str, title: &str, notes: &str) -> anyhow::Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO foreshadowings(id, novel_id, title, status, planted_chapter, notes, created_at, updated_at)
             VALUES(?1,?2,?3,'open',0,?4,?5,?5)",
            params![uuid::Uuid::new_v4().to_string(), novel_id, title, notes, now()],
        )?;
        Ok(())
    }

    pub fn update_foreshadowing(
        &self,
        novel_id: &str,
        id: &str,
        status: &str,
        notes: &str,
        resolved_chapter: Option<i64>,
    ) -> anyhow::Result<bool> {
        let conn = self.conn.lock().unwrap();
        let n = conn.execute(
            "UPDATE foreshadowings SET status=?2, notes=?3, resolved_chapter=?4, updated_at=?5
             WHERE id=?1 AND novel_id=?6",
            params![id, status, notes, resolved_chapter, now(), novel_id],
        )?;
        Ok(n > 0)
    }

    pub fn delete_foreshadowing(&self, novel_id: &str, id: &str) -> anyhow::Result<bool> {
        let conn = self.conn.lock().unwrap();
        Ok(conn.execute(
            "DELETE FROM foreshadowings WHERE id=?1 AND novel_id=?2",
            params![id, novel_id],
        )? > 0)
    }

    fn insert_foreshadowing_if_new(
        &self,
        novel_id: &str,
        title: &str,
        chapter_idx: i64,
    ) -> anyhow::Result<()> {
        let conn = self.conn.lock().unwrap();
        let ts = now();
        conn.execute(
            "INSERT INTO foreshadowings(id, novel_id, title, status, planted_chapter, notes, created_at, updated_at)
             SELECT ?1,?2,?3,'open',?4,'',?5,?5
             WHERE NOT EXISTS (SELECT 1 FROM foreshadowings WHERE novel_id=?2 AND title=?3)",
            params![uuid::Uuid::new_v4().to_string(), novel_id, title, chapter_idx, ts],
        )?;
        Ok(())
    }

    fn resolve_foreshadowing(&self, novel_id: &str, title: &str, chapter_idx: i64) -> anyhow::Result<usize> {
        let conn = self.conn.lock().unwrap();
        // 用 instr 而非 LIKE：伏笔标题里的 % / _ 不应被当作通配符
        let hit: Option<String> = conn
            .query_row(
                "SELECT id FROM foreshadowings
                 WHERE novel_id=?1 AND status='open'
                   AND (title=?2 OR instr(title, ?2) > 0 OR instr(?2, title) > 0)
                 LIMIT 1",
                params![novel_id, title],
                |r| r.get(0),
            )
            .optional()?;
        if let Some(id) = hit {
            conn.execute(
                "UPDATE foreshadowings SET status='resolved', resolved_chapter=?2, updated_at=?3 WHERE id=?1",
                params![id, chapter_idx, now()],
            )?;
            Ok(1)
        } else {
            Ok(0)
        }
    }

    /// 写后抽取：从 LLM JSON 吸收伏笔（新埋 / 回收）
    pub fn absorb_foreshadowings(
        &self,
        novel_id: &str,
        chapter_idx: i64,
        j: &serde_json::Value,
    ) -> anyhow::Result<usize> {
        let mut n = 0;
        if let Some(arr) = j["foreshadowings_planted"].as_array() {
            for item in arr {
                let title = item["title"].as_str().unwrap_or("").trim();
                if title.is_empty() {
                    continue;
                }
                self.insert_foreshadowing_if_new(novel_id, title, chapter_idx)?;
                n += 1;
            }
        }
        if let Some(arr) = j["foreshadowings_resolved"].as_array() {
            for item in arr {
                let title = item["title"].as_str().unwrap_or("").trim();
                if title.is_empty() {
                    continue;
                }
                n += self.resolve_foreshadowing(novel_id, title, chapter_idx)?;
            }
        }
        Ok(n)
    }

    // ---------- 章节结构编辑 ----------

    /// 统一重排：先整体挪到负区（避免 UNIQUE(novel_id, idx) 冲突），再按新顺序写回。
    /// 整个过程在事务里：中断则回滚，不会留下「写了一半」的负区状态。
    /// 版本历史跟随章节移动（idx 键需要重映射），已删除章节的版本一并清理。
    fn apply_chapter_order(
        conn: &Connection,
        novel_id: &str,
        order: &[(String, i64)],
    ) -> anyhow::Result<()> {
        conn.execute_batch("BEGIN IMMEDIATE")?;
        let result = (|| -> anyhow::Result<()> {
            // 记录每个章节的旧章号 → 新章号（供版本历史重映射）
            let mut moves: Vec<(i64, i64)> = Vec::new();
            for (id, new_idx) in order {
                let old: i64 = conn.query_row(
                    "SELECT idx FROM chapters WHERE id=?1",
                    [id],
                    |r| r.get(0),
                )?;
                moves.push((old, *new_idx));
            }
            // 版本先整体挪到负区（避免交换场景的二次命中），章号编码为 -(old) - 1_000_000
            conn.execute(
                "UPDATE chapter_versions SET idx = -idx - 1000000 WHERE novel_id=?1",
                params![novel_id],
            )?;
            conn.execute(
                "UPDATE chapters SET idx = -idx - 1 WHERE novel_id=?1",
                params![novel_id],
            )?;
            for (id, new_idx) in order {
                conn.execute(
                    "UPDATE chapters SET idx=?2, updated_at=?3 WHERE id=?1",
                    params![id, new_idx, now()],
                )?;
            }
            // 版本跟随：负区编码 → 新章号；映射后仍为负的 = 已删除章节的版本 → 清理
            for (old, new) in &moves {
                conn.execute(
                    "UPDATE chapter_versions SET idx=?2 WHERE novel_id=?1 AND idx=?3",
                    params![novel_id, new, -(old) - 1000000],
                )?;
            }
            conn.execute(
                "DELETE FROM chapter_versions WHERE novel_id=?1 AND idx < 0",
                params![novel_id],
            )?;
            // 其它派生数据跟随章号：伏笔台账引用、全局审校涉及章节、失效卷摘要
            let old_to_new: std::collections::HashMap<i64, i64> = moves
                .iter()
                .filter(|(o, _)| *o > 0)
                .map(|(o, n)| (*o, *n))
                .collect();
            let max_old = moves
                .iter()
                .map(|(o, _)| *o)
                .filter(|o| *o > 0)
                .max()
                .unwrap_or(0);
            Self::remap_foreshadow_chapters_locked(conn, novel_id, &old_to_new)?;
            Self::remap_global_review_locked(conn, novel_id, &old_to_new)?;
            // 卷摘要失效判定：
            // ① 中间有被删章号（幸存章号出现缺口）→ 缺口起点及之后覆盖的卷全部失效（内容已错位）
            // ② 卷覆盖范围超出新的章节总数 → 尾部被删的卷失效
            let new_total = order.len() as i64;
            if let Some(gap) = (1..=max_old).find(|i| !old_to_new.contains_key(i)) {
                conn.execute(
                    "DELETE FROM volume_summaries WHERE novel_id=?1 AND upto_chapter >= ?2",
                    params![novel_id, gap],
                )?;
                conn.execute(
                    "DELETE FROM volume_reports WHERE novel_id=?1 AND upto_chapter >= ?2",
                    params![novel_id, gap],
                )?;
                conn.execute(
                    "DELETE FROM book_reports WHERE novel_id=?1 AND kind='finish'",
                    params![novel_id],
                )?;
            }
            conn.execute(
                "DELETE FROM volume_summaries WHERE novel_id=?1 AND upto_chapter > ?2",
                params![novel_id, new_total],
            )?;
            conn.execute(
                "DELETE FROM volume_reports WHERE novel_id=?1 AND upto_chapter > ?2",
                params![novel_id, new_total],
            )?;
            conn.execute(
                "DELETE FROM book_reports WHERE novel_id=?1 AND kind='finish'",
                params![novel_id],
            )?;
            // 章号已整体重排：重建本书检索索引（索引里存的是旧章号）
            Self::reindex_novel_locked(conn, novel_id)?;
            Ok(())
        })();
        match result {
            Ok(()) => {
                conn.execute_batch("COMMIT")?;
                Ok(())
            }
            Err(e) => {
                let _ = conn.execute_batch("ROLLBACK");
                Err(e)
            }
        }
    }

    fn chapter_pairs_ordered(&self, conn: &Connection, novel_id: &str) -> anyhow::Result<Vec<(String, i64)>> {
        let mut stmt = conn.prepare("SELECT id, idx FROM chapters WHERE novel_id=?1 ORDER BY idx ASC")?;
        let rows = stmt.query_map([novel_id], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?;
        let mut v = Vec::new();
        for x in rows {
            v.push(x?);
        }
        Ok(v)
    }

    /// 章号映射：存活章号直接映射；被删章号就地落到「其前最近的存活章」，找不到则 None。
    fn remap_idx(old: i64, map: &std::collections::HashMap<i64, i64>) -> Option<i64> {
        if old <= 0 {
            return Some(old.max(0));
        }
        if let Some(n) = map.get(&old) {
            return Some(*n);
        }
        map.iter()
            .filter(|(o, _)| **o > 0 && **o < old)
            .max_by_key(|(o, _)| **o)
            .map(|(_, n)| *n)
    }

    /// 章号重排后：伏笔台账的埋设/回收章号跟随映射（被删章号落到其前最近存活章，找不到归 0/None）
    fn remap_foreshadow_chapters_locked(
        conn: &Connection,
        novel_id: &str,
        map: &std::collections::HashMap<i64, i64>,
    ) -> anyhow::Result<()> {
        let mut stmt = conn.prepare(
            "SELECT id, planted_chapter, resolved_chapter FROM foreshadowings WHERE novel_id=?1",
        )?;
        let rows: Vec<(String, i64, Option<i64>)> = stmt
            .query_map([novel_id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
            .collect::<Result<_, _>>()?;
        for (id, planted, resolved) in rows {
            let np = Self::remap_idx(planted, map).unwrap_or(0);
            let nr = resolved.and_then(|r| Self::remap_idx(r, map));
            conn.execute(
                "UPDATE foreshadowings SET planted_chapter=?2, resolved_chapter=?3, updated_at=?4 WHERE id=?1",
                params![id, np, nr, now()],
            )?;
        }
        Ok(())
    }

    /// 章号重排后：全局审校「涉及章节」跟随映射并去重（全部落空时保留空数组，等下一轮全局审校归并）
    fn remap_global_review_locked(
        conn: &Connection,
        novel_id: &str,
        map: &std::collections::HashMap<i64, i64>,
    ) -> anyhow::Result<()> {
        let raw: Option<String> = conn
            .query_row(
                "SELECT review_json FROM global_reviews WHERE novel_id=?1",
                [novel_id],
                |r| r.get(0),
            )
            .optional()?;
        let Some(raw) = raw else { return Ok(()) };
        let Ok(mut review) = serde_json::from_str::<crate::models::GlobalReview>(&raw) else {
            return Ok(());
        };
        let mut changed = false;
        for issue in review.issues.iter_mut() {
            let mut mapped: Vec<i64> = issue
                .affected_chapters
                .iter()
                .filter_map(|c| Self::remap_idx(*c, map))
                .collect();
            mapped.sort_unstable();
            mapped.dedup();
            if mapped != issue.affected_chapters {
                issue.affected_chapters = mapped;
                changed = true;
            }
        }
        if changed {
            conn.execute(
                "UPDATE global_reviews SET review_json=?2 WHERE novel_id=?1",
                params![novel_id, serde_json::to_string(&review)?],
            )?;
        }
        Ok(())
    }

    /// 按给定顺序把章节写回为 1..n（重排/插入/删除的统一入口）
    fn write_order(conn: &Connection, novel_id: &str, ids: &[String]) -> anyhow::Result<()> {
        let order: Vec<(String, i64)> = ids
            .iter()
            .enumerate()
            .map(|(i, id)| (id.clone(), i as i64 + 1))
            .collect();
        Self::apply_chapter_order(conn, novel_id, &order)
    }

    /// 在 after_idx 之后插入一章（after_idx=0 表示插到最前），返回新章 idx。
    pub fn insert_chapter_after(
        &self,
        novel_id: &str,
        after_idx: i64,
        title: &str,
        goal: &str,
    ) -> anyhow::Result<i64> {
        let conn = self.conn.lock().unwrap();
        let pairs = self.chapter_pairs_ordered(&conn, novel_id)?;
        let pos = pairs.iter().filter(|(_, i)| *i <= after_idx).count().min(pairs.len());
        let mut ids: Vec<String> = pairs.into_iter().map(|(id, _)| id).collect();

        let new_id = uuid::Uuid::new_v4().to_string();
        let ts = now();
        conn.execute(
            "INSERT INTO chapters(id, novel_id, idx, title, goal, beats_json, content, status, created_at, updated_at)
             VALUES(?1,?2,-1000000,?3,?4,'[]','','planned',?5,?5)",
            params![new_id, novel_id, title, goal, ts],
        )?;
        ids.insert(pos, new_id);
        Self::write_order(&conn, novel_id, &ids)?;
        Ok(pos as i64 + 1)
    }

    /// 删除一章并重排；索引越界或仅剩一章时返回 false。
    pub fn delete_chapter_at(&self, novel_id: &str, idx: i64) -> anyhow::Result<bool> {
        let conn = self.conn.lock().unwrap();
        let pairs = self.chapter_pairs_ordered(&conn, novel_id)?;
        let mut ids: Vec<String> = pairs.into_iter().map(|(id, _)| id).collect();
        if ids.len() <= 1 || idx < 1 || idx > ids.len() as i64 {
            return Ok(false);
        }
        let removed = ids.remove(idx as usize - 1);
        conn.execute("DELETE FROM chapters WHERE id=?1", [removed])?;
        Self::write_order(&conn, novel_id, &ids)?;
        Ok(true)
    }

    /// 批量删除章号区间 [from, to] 并重排；至少保留一章，否则报错。
    pub fn batch_delete_chapters(&self, novel_id: &str, from: i64, to: i64) -> anyhow::Result<usize> {
        let conn = self.conn.lock().unwrap();
        let pairs = self.chapter_pairs_ordered(&conn, novel_id)?;
        let total = pairs.len() as i64;
        if from < 1 || to < from || to > total {
            anyhow::bail!("范围不合法（当前共 {total} 章）");
        }
        let deleted = to - from + 1;
        if total - deleted < 1 {
            anyhow::bail!("至少要保留一章");
        }
        let ids: Vec<String> = pairs
            .into_iter()
            .filter(|(_, idx)| *idx < from || *idx > to)
            .map(|(id, _)| id)
            .collect();
        // 先删行，再重排（重排会同步版本历史与检索索引）
        conn.execute(
            "DELETE FROM chapters WHERE novel_id=?1 AND idx >= ?2 AND idx <= ?3",
            params![novel_id, from, to],
        )?;
        Self::write_order(&conn, novel_id, &ids)?;
        Ok(deleted as usize)
    }

    /// 清空第 after 章之后的正文（保留章纲）：旧稿进版本历史，状态收回 planned，摘要/审校一并清空。
    /// 返回受影响的章节数。
    pub fn clear_chapters_after(&self, novel_id: &str, after: i64) -> anyhow::Result<usize> {
        let conn = self.conn.lock().unwrap();
        let pairs = self.chapter_pairs_ordered(&conn, novel_id)?;
        let mut n = 0usize;
        for (id, idx) in pairs {
            if idx <= after {
                continue;
            }
            Self::snapshot_chapter_version_locked(&conn, novel_id, idx, "rewrite-after", "")?;
            conn.execute(
                "UPDATE chapters SET content='', status='planned', summary='', review_json=NULL,
                  style_report='', updated_at=?2 WHERE id=?1",
                params![id, now()],
            )?;
            n += 1;
        }
        // 覆盖到的卷摘要失效（内容将被重写），等待下一批自动重建
        conn.execute(
            "DELETE FROM volume_summaries WHERE novel_id=?1 AND upto_chapter > ?2",
            params![novel_id, after],
        )?;
        conn.execute(
            "DELETE FROM volume_reports WHERE novel_id=?1 AND upto_chapter > ?2",
            params![novel_id, after],
        )?;
        conn.execute(
            "DELETE FROM book_reports WHERE novel_id=?1 AND kind='finish'",
            params![novel_id],
        )?;
        Self::reindex_novel_locked(&conn, novel_id)?;
        Ok(n)
    }

    /// 删除第 after 章之后的全部章节（「重写并重新规划」用）：旧稿先进版本历史再删行。
    /// 版本历史保留（重新生成后仍可查看/回滚到上一代稿）。返回删除的章节数。
    pub fn delete_chapters_after(&self, novel_id: &str, after: i64) -> anyhow::Result<usize> {
        let conn = self.conn.lock().unwrap();
        let pairs = self.chapter_pairs_ordered(&conn, novel_id)?;
        let mut n = 0usize;
        for (_id, idx) in pairs.iter() {
            if *idx <= after {
                continue;
            }
            Self::snapshot_chapter_version_locked(&conn, novel_id, *idx, "rewrite-after", "")?;
            n += 1;
        }
        conn.execute(
            "DELETE FROM chapters WHERE novel_id=?1 AND idx > ?2",
            params![novel_id, after],
        )?;
        conn.execute(
            "DELETE FROM chapter_fts WHERE novel_id=?1 AND chapter_idx > ?2",
            params![novel_id, after],
        )?;
        // 覆盖到的卷摘要失效（内容将被重新规划与重写）
        conn.execute(
            "DELETE FROM volume_summaries WHERE novel_id=?1 AND upto_chapter > ?2",
            params![novel_id, after],
        )?;
        conn.execute(
            "DELETE FROM volume_reports WHERE novel_id=?1 AND upto_chapter > ?2",
            params![novel_id, after],
        )?;
        conn.execute(
            "DELETE FROM book_reports WHERE novel_id=?1 AND kind='finish'",
            params![novel_id],
        )?;
        Ok(n)
    }

    /// 上下移动一章（direction: -1 上移 / 1 下移）；到边界返回 false。
    pub fn move_chapter(&self, novel_id: &str, idx: i64, direction: i64) -> anyhow::Result<bool> {
        let conn = self.conn.lock().unwrap();
        let pairs = self.chapter_pairs_ordered(&conn, novel_id)?;
        let mut ids: Vec<String> = pairs.into_iter().map(|(id, _)| id).collect();
        let n = ids.len() as i64;
        let target = idx + direction;
        if idx < 1 || idx > n || target < 1 || target > n {
            return Ok(false);
        }
        ids.swap(idx as usize - 1, target as usize - 1);
        Self::write_order(&conn, novel_id, &ids)?;
        Ok(true)
    }

    // ---------- LLM 用量 ----------

    pub fn insert_usage(&self, r: &UsageRecord) -> anyhow::Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO llm_usage(id, novel_id, chapter_idx, task, provider, model,
                 input_tokens, output_tokens, cache_hit_tokens, cache_miss_tokens, total_tokens, created_at)
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)",
            params![
                r.id, r.novel_id, r.chapter_idx, r.task, r.provider, r.model,
                r.input, r.output, r.cache_hit, r.cache_miss, r.total, r.created_at
            ],
        )?;
        Ok(())
    }

    fn map_usage_totals(row: &Row<'_>, offset: usize) -> rusqlite::Result<UsageTotals> {
        Ok(UsageTotals {
            calls: row.get(offset)?,
            known_input: row.get(offset + 1)?,
            input: row.get(offset + 2)?,
            output: row.get(offset + 3)?,
            cache_hit: row.get(offset + 4)?,
            cache_miss: row.get(offset + 5)?,
            total: row.get(offset + 6)?,
        })
    }

    /// 汇总（novel_id=None 时统计全部；SUM 为 NULL 表示全部记录都缺该字段 → 未知）
    pub fn usage_totals(&self, novel_id: Option<&str>) -> anyhow::Result<UsageTotals> {
        let conn = self.conn.lock().unwrap();
        let row = conn.query_row(
            "SELECT COUNT(*), COUNT(input_tokens), SUM(input_tokens), SUM(output_tokens),
                    SUM(cache_hit_tokens), SUM(cache_miss_tokens), SUM(total_tokens)
             FROM llm_usage WHERE (?1 IS NULL OR novel_id = ?1)",
            params![novel_id],
            |r| Self::map_usage_totals(r, 0),
        )?;
        Ok(row)
    }

    pub fn usage_by_task(&self, novel_id: &str) -> anyhow::Result<Vec<UsageGroup>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT task, COUNT(*), COUNT(input_tokens), SUM(input_tokens), SUM(output_tokens),
                    SUM(cache_hit_tokens), SUM(cache_miss_tokens), SUM(total_tokens)
             FROM llm_usage WHERE novel_id=?1
             GROUP BY task ORDER BY COALESCE(SUM(total_tokens), 0) DESC, task ASC",
        )?;
        let rows = stmt.query_map([novel_id], |r| {
            Ok(UsageGroup {
                key: r.get(0)?,
                totals: Self::map_usage_totals(r, 1)?,
            })
        })?;
        let mut out = Vec::new();
        for x in rows {
            out.push(x?);
        }
        Ok(out)
    }

    pub fn usage_by_chapter(&self, novel_id: &str) -> anyhow::Result<Vec<UsageGroup>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT chapter_idx, COUNT(*), COUNT(input_tokens), SUM(input_tokens), SUM(output_tokens),
                    SUM(cache_hit_tokens), SUM(cache_miss_tokens), SUM(total_tokens)
             FROM llm_usage WHERE novel_id=?1 AND chapter_idx IS NOT NULL
             GROUP BY chapter_idx ORDER BY chapter_idx ASC",
        )?;
        let rows = stmt.query_map([novel_id], |r| {
            let idx: i64 = r.get(0)?;
            Ok(UsageGroup {
                key: format!("第{idx}章"),
                totals: Self::map_usage_totals(r, 1)?,
            })
        })?;
        let mut out = Vec::new();
        for x in rows {
            out.push(x?);
        }
        Ok(out)
    }

    // ---------- 备份 / 恢复 ----------

    /// 生成一致性快照（VACUUM INTO）
    pub fn vacuum_into(&self, path: &std::path::Path) -> anyhow::Result<()> {
        if path.exists() {
            std::fs::remove_file(path)?;
        }
        let conn = self.conn.lock().unwrap();
        conn.execute("VACUUM INTO ?1", [path.to_string_lossy().to_string()])?;
        Ok(())
    }

    /// 用备份文件覆盖当前数据库（SQLite backup API，进程内完成，无需重启）
    pub fn restore_from_file(&self, path: &std::path::Path) -> anyhow::Result<()> {
        let src = Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        {
            let mut dst = self.conn.lock().unwrap();
            let backup = rusqlite::backup::Backup::new(&src, &mut dst)?;
            backup.run_to_completion(
                1000,
                std::time::Duration::from_millis(0),
                None::<fn(rusqlite::backup::Progress)>,
            )?;
        }
        // 恢复后补跑轻量迁移（兼容更早版本的备份），并把备份里可能残留的 running 运行标记为失败
        self.migrate()?;
        self.recover_stale_runs()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_dir(tag: &str) -> std::path::PathBuf {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("ainovel-db-{tag}-{stamp}"))
    }

    #[test]
    fn stale_running_runs_are_recovered_on_open() {
        let dir = tmp_dir("recover");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("ainovel.db");

        let novel_id;
        {
            let db = Db::open(&path).unwrap();
            let novel = db.create_novel("中断恢复测试", "auto", "").unwrap();
            novel_id = novel.id.clone();
            db.create_run(&novel.id, 3).unwrap(); // 状态 running
            let run = db.latest_run(&novel.id).unwrap().unwrap();
            assert_eq!(run.status, "running");
        }

        // 模拟进程重启：重新打开（不清理任何状态）
        let db2 = Db::open(&path).unwrap();
        let run = db2.latest_run(&novel_id).unwrap().unwrap();
        assert_eq!(run.status, "failed", "重启后孤儿运行应被标记失败");
        assert!(
            run.error.unwrap_or_default().contains("进程中断"),
            "应留下可读原因"
        );

        // 再次打开不应重复标记（状态已不是 running）
        let db3 = Db::open(&path).unwrap();
        assert_eq!(db3.latest_run(&novel_id).unwrap().unwrap().status, "failed");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn fts_search_matches_chinese_substring_and_syncs_on_edit() {
        use crate::models::ChapterPlan;
        let dir = tmp_dir("fts");
        std::fs::create_dir_all(&dir).unwrap();
        let db = Db::open(&dir.join("ainovel.db")).unwrap();
        let novel = db.create_novel("检索测试", "auto", "").unwrap();
        db.upsert_chapter_plan(
            &novel.id,
            &ChapterPlan { idx: 1, title: "青铜钥匙".into(), goal: String::new(), beats: vec![] },
        )
        .unwrap();
        db.save_chapter_content(
            &novel.id,
            1,
            "他在旧宅的抽屉里发现了一把青铜钥匙，钥匙上刻着鸢尾花纹。",
            "done",
        )
        .unwrap();
        db.upsert_chapter_plan(
            &novel.id,
            &ChapterPlan { idx: 2, title: "远航".into(), goal: String::new(), beats: vec![] },
        )
        .unwrap();
        db.save_chapter_content(&novel.id, 2, "海面上起了雾，灯塔的光在雾里散成一片。", "done")
            .unwrap();

        // 中文子串（多字词）检索：命中且带上下文摘录
        let hits = db.search_chapters(&novel.id, "青铜钥匙", 10).unwrap();
        assert_eq!(hits.len(), 1, "{hits:?}");
        assert_eq!(hits[0].0, 1);
        assert_eq!(hits[0].1, "青铜钥匙");
        assert!(hits[0].2.contains("青铜钥匙"), "摘录应包含命中词：{}", hits[0].2);
        // 另一个词命中第二章
        let hits2 = db.search_chapters(&novel.id, "灯塔", 10).unwrap();
        assert_eq!(hits2.len(), 1);
        assert_eq!(hits2[0].0, 2);
        // 章名也可检索
        let hits3 = db.search_chapters(&novel.id, "远航", 10).unwrap();
        assert!(hits3.iter().any(|h| h.0 == 2), "{hits3:?}");
        // 无命中
        assert!(db.search_chapters(&novel.id, "完全不存在的词组", 10).unwrap().is_empty());

        // 改写正文后索引同步：仅存在于旧正文的词消失；标题仍在索引（章节名可检索）
        db.save_chapter_content(&novel.id, 1, "抽屉里已经什么都没有了。", "done")
            .unwrap();
        assert!(
            db.search_chapters(&novel.id, "鸢尾花纹", 10).unwrap().is_empty(),
            "旧正文中的词应从索引消失"
        );
        assert!(!db.search_chapters(&novel.id, "抽屉", 10).unwrap().is_empty());
        assert!(
            db.search_chapters(&novel.id, "青铜钥匙", 10).unwrap().iter().any(|h| h.0 == 1),
            "章节名仍在索引中"
        );

        // 章节重排后章号同步
        db.move_chapter(&novel.id, 1, 1).unwrap();
        let hits4 = db.search_chapters(&novel.id, "灯塔", 10).unwrap();
        assert!(hits4.iter().any(|h| h.0 == 1), "重排后应指向新章号：{hits4:?}");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn review_fixed_cap_keeps_open_and_latest_fixed() {
        use crate::models::{cap_review_fixed, Review, ReviewIssue};
        let mut issues: Vec<ReviewIssue> = (0..15)
            .map(|i| ReviewIssue {
                severity: "low".into(),
                note: format!("fixed-{i}"),
                quote: String::new(),
                status: "fixed".into(),
                is_new: false,
                stale: false,
            })
            .collect();
        issues.push(ReviewIssue {
            severity: "high".into(),
            note: "open-1".into(),
            quote: String::new(),
            status: "open".into(),
            is_new: false,
            stale: false,
        });
        let mut review = Review { ok: false, issues, content_hash: String::new() };
        cap_review_fixed(&mut review, 10);
        assert_eq!(review.issues.iter().filter(|i| i.status == "fixed").count(), 10);
        assert_eq!(review.issues.iter().filter(|i| i.status == "open").count(), 1);
        let notes: Vec<&str> = review
            .issues
            .iter()
            .filter(|i| i.status == "fixed")
            .map(|i| i.note.as_str())
            .collect();
        assert!(notes.contains(&"fixed-14"), "应保留最新的 fixed");
        assert!(!notes.contains(&"fixed-0"), "应丢弃最早的 fixed");

        // 上游 save_chapter_review 走同一裁剪（写库后再读）
        let dir = tmp_dir("cap");
        std::fs::create_dir_all(&dir).unwrap();
        let db = Db::open(&dir.join("ainovel.db")).unwrap();
        let novel = db.create_novel("留档上限测试", "auto", "").unwrap();
        db.upsert_chapter_plan(
            &novel.id,
            &crate::models::ChapterPlan { idx: 1, title: "第一章".into(), goal: String::new(), beats: vec![] },
        )
        .unwrap();
        let big = Review {
            ok: true,
            issues: (0..13)
                .map(|i| ReviewIssue {
                    severity: "low".into(),
                    note: format!("f-{i}"),
                    quote: String::new(),
                    status: "fixed".into(),
                    is_new: false,
                    stale: false,
                })
                .collect(),
            content_hash: String::new(),
        };
        db.save_chapter_review(&novel.id, 1, &big).unwrap();
        let stored = db.get_chapter(&novel.id, 1).unwrap().unwrap().review_json.unwrap();
        let parsed: Review = serde_json::from_str(&stored).unwrap();
        assert_eq!(parsed.issues.len(), 10, "入库时 fixed 应裁剪到 10 条");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn review_staleness_refreshes_when_content_changes() {
        use crate::models::{ChapterPlan, Review, ReviewIssue};
        let dir = tmp_dir("stale");
        std::fs::create_dir_all(&dir).unwrap();
        let db = Db::open(&dir.join("ainovel.db")).unwrap();
        let novel = db.create_novel("引用失效测试", "auto", "").unwrap();
        db.upsert_chapter_plan(
            &novel.id,
            &ChapterPlan { idx: 1, title: "第一章".into(), goal: String::new(), beats: vec![] },
        )
        .unwrap();
        db.save_chapter_content(&novel.id, 1, "他把青铜钥匙插进锁孔，轻轻一转。", "done")
            .unwrap();
        let review = Review {
            ok: false,
            issues: vec![ReviewIssue {
                severity: "medium".into(),
                note: "节奏".into(),
                quote: "把青铜钥匙插进锁孔".into(),
                status: "open".into(),
                is_new: false,
                stale: false,
            }],
            content_hash: String::new(),
        };
        db.save_chapter_review(&novel.id, 1, &review).unwrap();
        // 保存正文（同内容）不改变标记
        db.save_chapter_content(&novel.id, 1, "他把青铜钥匙插进锁孔，轻轻一转。", "done")
            .unwrap();
        let stored = db.get_chapter(&novel.id, 1).unwrap().unwrap().review_json.unwrap();
        let parsed: Review = serde_json::from_str(&stored).unwrap();
        assert!(!parsed.issues[0].stale, "引用仍能定位，不应标记");
        // 手动改正文 → 引用失效，自动标记 stale
        db.save_chapter_edit(&novel.id, 1, Some("窗外雨停了。"), None).unwrap();
        let stored = db.get_chapter(&novel.id, 1).unwrap().unwrap().review_json.unwrap();
        let parsed: Review = serde_json::from_str(&stored).unwrap();
        assert!(parsed.issues[0].stale, "引用对不上后应标记 stale");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn negative_index_recovery_survives_conflict_and_heals_fts() {
        use crate::models::ChapterPlan;
        let dir = tmp_dir("negidx");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("ainovel.db");
        let novel_id;
        {
            let db = Db::open(&path).unwrap();
            let novel = db.create_novel("重排中断测试", "auto", "").unwrap();
            novel_id = novel.id.clone();
            db.upsert_chapter_plan(
                &novel.id,
                &ChapterPlan { idx: 1, title: "第一章".into(), goal: String::new(), beats: vec![] },
            )
            .unwrap();
            db.save_chapter_content(&novel.id, 1, "灯塔的光扫过海面。", "done").unwrap();
            db.upsert_chapter_plan(
                &novel.id,
                &ChapterPlan { idx: 2, title: "第二章".into(), goal: String::new(), beats: vec![] },
            )
            .unwrap();
            db.save_chapter_content(&novel.id, 2, "船舱里只剩下风声。", "done").unwrap();
        }
        // 模拟旧版本「重排写到一半崩溃」：A 变 -2（应还原为 1），B 已写成 1（占位冲突）
        {
            let conn = rusqlite::Connection::open(&path).unwrap();
            conn.execute_batch(
                "UPDATE chapters SET idx=-2 WHERE idx=1;
                 UPDATE chapters SET idx=1 WHERE idx=2;",
            )
            .unwrap();
        }
        // 旧实现会在这里 UNIQUE 冲突 → 引擎起不来；现在必须能启动并自愈
        let db2 = Db::open(&path).unwrap();
        let metas = db2.list_chapter_metas(&novel_id).unwrap();
        assert_eq!(metas.len(), 2);
        assert!(
            metas.iter().all(|m| m.idx > 0),
            "还原后章号必须全为正：{:?}",
            metas.iter().map(|m| m.idx).collect::<Vec<_>>()
        );
        // FTS 与章号一致：按标题检索出的章号要能打开同名章节
        let hits = db2.search_chapters(&novel_id, "第一章", 10).unwrap();
        assert_eq!(hits.len(), 1, "{hits:?}");
        let (hit_idx, hit_title, _) = &hits[0];
        assert_eq!(hit_title, "第一章");
        let ch = db2.get_chapter(&novel_id, *hit_idx).unwrap().unwrap();
        assert_eq!(ch.title, "第一章", "检索章号与 chapters 表必须对应");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn create_run_rejects_second_running_run() {
        let dir = tmp_dir("runlock");
        std::fs::create_dir_all(&dir).unwrap();
        let db = Db::open(&dir.join("ainovel.db")).unwrap();
        let novel = db.create_novel("并发运行测试", "auto", "").unwrap();
        db.create_run(&novel.id, 3).unwrap();
        let err = db.create_run(&novel.id, 3).unwrap_err();
        assert!(format!("{err}").contains("已有创作运行"), "{err}");
        // 前一个运行结束后可以再开
        let run = db.latest_run(&novel.id).unwrap().unwrap();
        db.update_run(&run.id, "done", "done", 3, None).unwrap();
        assert!(db.create_run(&novel.id, 3).is_ok());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn absorb_characters_preserves_manual_status() {
        let dir = tmp_dir("charstat");
        std::fs::create_dir_all(&dir).unwrap();
        let db = Db::open(&dir.join("ainovel.db")).unwrap();
        let novel = db.create_novel("角色状态测试", "auto", "").unwrap();
        db.upsert_character(&novel.id, "林深", "主角", "dead", &[]).unwrap();
        // 写后抽取没给 status：不能把人工标记的 dead 重置为 active
        let j = serde_json::json!({
            "characters": [{"name": "林深", "role": "主角", "facts": ["找到录音笔"]}]
        });
        db.absorb_characters(&novel.id, &j).unwrap();
        let cs = db.list_characters(&novel.id).unwrap();
        assert_eq!(cs.len(), 1);
        assert_eq!(cs[0].status, "dead", "缺 status 时应保留台账原值");
        // 抽取明确给出 status 时以新值为准
        let j2 = serde_json::json!({
            "characters": [{"name": "林深", "role": "主角", "status": "active", "facts": []}]
        });
        db.absorb_characters(&novel.id, &j2).unwrap();
        assert_eq!(db.list_characters(&novel.id).unwrap()[0].status, "active");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn foreshadow_resolution_does_not_treat_percent_as_wildcard() {
        let dir = tmp_dir("forepat");
        std::fs::create_dir_all(&dir).unwrap();
        let db = Db::open(&dir.join("ainovel.db")).unwrap();
        let novel = db.create_novel("伏笔匹配测试", "auto", "").unwrap();
        db.add_foreshadowing(&novel.id, "50%的真相", "").unwrap();
        db.add_foreshadowing(&novel.id, "他们走了50步", "").unwrap();
        // 回收标题含 %：只能命中真正包含「50%」的那条
        let j = serde_json::json!({"foreshadowings_resolved": [{"title": "50%"}]});
        db.absorb_foreshadowings(&novel.id, 3, &j).unwrap();
        let items = db.list_foreshadowings(&novel.id).unwrap();
        let hit = items.iter().find(|f| f.title == "50%的真相").unwrap();
        assert_eq!(hit.status, "resolved", "应命中 50%的真相");
        let other = items.iter().find(|f| f.title == "他们走了50步").unwrap();
        assert_eq!(other.status, "open", "不应把 % 当通配符误回收别的伏笔");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn chapter_versions_snapshot_retention_and_restore() {
        use crate::models::{ChapterPlan, Review, ReviewIssue};
        let dir = tmp_dir("versions");
        std::fs::create_dir_all(&dir).unwrap();
        let db = Db::open(&dir.join("ainovel.db")).unwrap();
        let novel = db.create_novel("版本历史测试", "auto", "").unwrap();
        db.upsert_chapter_plan(
            &novel.id,
            &ChapterPlan { idx: 1, title: "第一章".into(), goal: String::new(), beats: vec![] },
        )
        .unwrap();
        // 首次写入：旧内容为空，不应留档
        db.save_chapter_content(&novel.id, 1, "第一版正文", "done").unwrap();
        assert!(db.list_chapter_versions(&novel.id, 1).unwrap().is_empty());

        // 给第一版配一条审校（带 1 条 high open），随后覆盖 → 应留档且带审校快照
        let review = Review {
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
        db.save_chapter_review(&novel.id, 1, &review).unwrap();
        db.save_chapter_content_sourced(&novel.id, 1, "第二版正文（更差）", "done", "manual").unwrap();
        let versions = db.list_chapter_versions(&novel.id, 1).unwrap();
        assert_eq!(versions.len(), 1);
        assert_eq!(versions[0].source, "pre-manual");
        assert!(versions[0].review_json.as_deref().unwrap().contains("开场冲突不足"));

        // 复原：恢复第一版与当时的审校；当前稿（第二版）自动留档为 pre-restore
        let vid = versions[0].id.clone();
        assert!(db.restore_chapter_version(&novel.id, 1, &vid, "restore").unwrap());
        let ch = db.get_chapter(&novel.id, 1).unwrap().unwrap();
        assert_eq!(ch.content, "第一版正文");
        assert!(ch.review_json.unwrap().contains("开场冲突不足"));
        let versions = db.list_chapter_versions(&novel.id, 1).unwrap();
        assert!(versions.iter().any(|v| v.source == "pre-restore" && v.content.contains("第二版")));

        // 保留上限：连续覆盖 12 次 → 最多 10 版
        for i in 0..12 {
            db.save_chapter_content_sourced(&novel.id, 1, &format!("第{i}轮"), "done", "manual")
                .unwrap();
        }
        assert_eq!(db.list_chapter_versions(&novel.id, 1).unwrap().len(), 10);
        // 跨书版本不可见（作用域正确）
        let other = db.create_novel("另一本", "auto", "").unwrap();
        db.upsert_chapter_plan(
            &other.id,
            &ChapterPlan { idx: 1, title: "第一章".into(), goal: String::new(), beats: vec![] },
        )
        .unwrap();
        assert!(db.get_chapter_version(&other.id, 1, &vid).unwrap().is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn move_chapter_keeps_versions_with_their_chapter() {
        use crate::models::ChapterPlan;
        let dir = tmp_dir("mvver");
        std::fs::create_dir_all(&dir).unwrap();
        let db = Db::open(&dir.join("ainovel.db")).unwrap();
        let novel = db.create_novel("版本跟随移动测试", "auto", "").unwrap();
        for (i, title) in [(1i64, "甲"), (2, "乙")] {
            db.upsert_chapter_plan(
                &novel.id,
                &ChapterPlan { idx: i, title: title.into(), goal: String::new(), beats: vec![] },
            )
            .unwrap();
            db.save_chapter_content(&novel.id, i, &format!("{title}章第一版"), "done").unwrap();
            db.save_chapter_content_sourced(&novel.id, i, &format!("{title}章第二版"), "done", "manual")
                .unwrap();
        }
        // 交换：现在 idx1 = 乙，idx2 = 甲；版本必须跟着章节走
        db.move_chapter(&novel.id, 1, 1).unwrap();
        assert_eq!(db.get_chapter(&novel.id, 1).unwrap().unwrap().title, "乙");
        assert_eq!(db.get_chapter(&novel.id, 2).unwrap().unwrap().title, "甲");
        let v1 = db.list_chapter_versions(&novel.id, 1).unwrap();
        assert!(v1.iter().any(|v| v.content == "乙章第一版"), "{v1:?}");
        assert!(!v1.iter().any(|v| v.content.contains("甲章")), "版本不得串章：{v1:?}");
        let v2 = db.list_chapter_versions(&novel.id, 2).unwrap();
        assert!(v2.iter().any(|v| v.content == "甲章第一版"), "{v2:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn batch_delete_renumbers_and_drops_deleted_versions() {
        use crate::models::ChapterPlan;
        let dir = tmp_dir("batchdel");
        std::fs::create_dir_all(&dir).unwrap();
        let db = Db::open(&dir.join("ainovel.db")).unwrap();
        let novel = db.create_novel("批量删除测试", "auto", "").unwrap();
        for i in 1..=4i64 {
            db.upsert_chapter_plan(
                &novel.id,
                &ChapterPlan { idx: i, title: format!("第{i}章"), goal: String::new(), beats: vec![] },
            )
            .unwrap();
            db.save_chapter_content(&novel.id, i, &format!("第{i}章第一版"), "done").unwrap();
            db.save_chapter_content_sourced(&novel.id, i, &format!("第{i}章第二版"), "done", "manual")
                .unwrap();
        }
        // 删除 2..3 → 原 4 重排为 2
        let deleted = db.batch_delete_chapters(&novel.id, 2, 3).unwrap();
        assert_eq!(deleted, 2);
        assert_eq!(db.get_chapter(&novel.id, 2).unwrap().unwrap().title, "第4章");
        // 原第 4 章的版本跟随到新章号 2；被删章节的版本已清理
        let v2 = db.list_chapter_versions(&novel.id, 2).unwrap();
        assert!(v2.iter().any(|v| v.content == "第4章第一版"), "{v2:?}");
        assert!(!v2.iter().any(|v| v.content.contains("第2章") || v.content.contains("第3章")));
        assert!(db.list_chapter_versions(&novel.id, 3).unwrap().is_empty());
        // 边界：范围非法 / 不能全删
        assert!(db.batch_delete_chapters(&novel.id, 2, 1).is_err());
        assert!(db.batch_delete_chapters(&novel.id, 1, 9).is_err());
        assert!(db.batch_delete_chapters(&novel.id, 1, 2).is_err(), "至少要保留一章");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn rewrite_after_clears_tail_or_removes_it_with_versions() {
        use crate::models::ChapterPlan;
        let dir = tmp_dir("rewafter");
        std::fs::create_dir_all(&dir).unwrap();
        let db = Db::open(&dir.join("ainovel.db")).unwrap();
        let novel = db.create_novel("后续重写测试", "auto", "").unwrap();
        for i in 1..=3i64 {
            db.upsert_chapter_plan(
                &novel.id,
                &ChapterPlan { idx: i, title: format!("第{i}章"), goal: format!("目标{i}"), beats: vec![] },
            )
            .unwrap();
            db.save_chapter_content(&novel.id, i, &format!("第{i}章正文"), "done").unwrap();
            db.set_chapter_summary(&novel.id, i, &format!("第{i}章摘要")).unwrap();
        }
        // 非 replan：清空第 2 章之后（章纲保留）
        let n = db.clear_chapters_after(&novel.id, 1).unwrap();
        assert_eq!(n, 2);
        let ch2 = db.get_chapter(&novel.id, 2).unwrap().unwrap();
        assert_eq!(ch2.content, "");
        assert_eq!(ch2.status, "planned");
        assert!(ch2.summary.is_empty(), "摘要应清空（由重写后的抽取重建）");
        assert!(ch2.review_json.is_none());
        assert_eq!(ch2.title, "第2章", "章纲保留");
        assert_eq!(ch2.goal, "目标2");
        let vs = db.list_chapter_versions(&novel.id, 2).unwrap();
        assert!(
            vs.iter().any(|v| v.source == "pre-rewrite-after" && v.content == "第2章正文"),
            "旧稿应留档可回滚：{vs:?}"
        );
        // replan：删除第 1 章之后
        let n2 = db.delete_chapters_after(&novel.id, 1).unwrap();
        assert_eq!(n2, 2);
        assert!(db.get_chapter(&novel.id, 2).unwrap().is_none());
        let metas = db.list_chapter_metas(&novel.id).unwrap();
        assert_eq!(metas.len(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn delete_renumbers_remap_derived_chapter_refs() {
        use crate::models::{ChapterPlan, GlobalIssue, GlobalReview};
        let dir = tmp_dir("remap-del");
        std::fs::create_dir_all(&dir).unwrap();
        let db = Db::open(&dir.join("ainovel.db")).unwrap();
        let novel = db.create_novel("删除引用同步测试", "auto", "").unwrap();
        for i in 1..=4i64 {
            db.upsert_chapter_plan(
                &novel.id,
                &ChapterPlan { idx: i, title: format!("第{i}章"), goal: String::new(), beats: vec![] },
            )
            .unwrap();
            db.save_chapter_content(&novel.id, i, &format!("第{i}章正文"), "done").unwrap();
        }
        // 伏笔：A 埋于第2章（将被删）；B 埋于第4章（存活 → 重排为2）；C 埋于1、回收于3（回收章将被删）
        db.absorb_foreshadowings(
            &novel.id,
            2,
            &serde_json::json!({"foreshadowings_planted": [{"title": "伏笔A"}]}),
        )
        .unwrap();
        db.absorb_foreshadowings(
            &novel.id,
            4,
            &serde_json::json!({"foreshadowings_planted": [{"title": "伏笔B"}]}),
        )
        .unwrap();
        db.absorb_foreshadowings(
            &novel.id,
            1,
            &serde_json::json!({"foreshadowings_planted": [{"title": "伏笔C"}]}),
        )
        .unwrap();
        db.absorb_foreshadowings(
            &novel.id,
            3,
            &serde_json::json!({"foreshadowings_resolved": [{"title": "伏笔C"}]}),
        )
        .unwrap();
        // 全局审校：涉及 2、3、4
        db.save_global_review(
            &novel.id,
            &GlobalReview {
                ok: true,
                issues: vec![GlobalIssue {
                    severity: "medium".into(),
                    note: "连续三章平淡".into(),
                    affected_chapters: vec![2, 3, 4],
                    suggestion: String::new(),
                    status: "open".into(),
                    is_new: false,
                }],
                updated_at: "t".into(),
            },
        )
        .unwrap();
        // 卷摘要覆盖到第 4 章
        db.upsert_volume_summary(&novel.id, 1, 4, "第1卷摘要").unwrap();

        // 删除 2..3 → 重排
        db.batch_delete_chapters(&novel.id, 2, 3).unwrap();

        let fs = db.list_foreshadowings(&novel.id).unwrap();
        let find = |t: &str| fs.iter().find(|f| f.title == t).unwrap();
        assert_eq!(find("伏笔A").planted_chapter, 1, "被删埋设章应就近落到前一个存活章");
        assert_eq!(find("伏笔B").planted_chapter, 2, "存活章号重排 4→2");
        assert_eq!(find("伏笔C").planted_chapter, 1);
        assert_eq!(
            find("伏笔C").resolved_chapter,
            Some(1),
            "被删回收章应就近落到前一个存活章"
        );
        let gr = db.get_global_review(&novel.id).unwrap().unwrap();
        assert_eq!(
            gr.issues[0].affected_chapters,
            vec![1, 2],
            "[2,3,4] 应映射为 [1,1,2] 并去重排序"
        );
        assert!(
            db.list_volume_summaries(&novel.id).unwrap().is_empty(),
            "被删章号覆盖的卷摘要应失效清理"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn move_remaps_derived_chapter_refs() {
        use crate::models::{ChapterPlan, GlobalIssue, GlobalReview};
        let dir = tmp_dir("remap-mv");
        std::fs::create_dir_all(&dir).unwrap();
        let db = Db::open(&dir.join("ainovel.db")).unwrap();
        let novel = db.create_novel("移动引用同步测试", "auto", "").unwrap();
        for i in 1..=2i64 {
            db.upsert_chapter_plan(
                &novel.id,
                &ChapterPlan { idx: i, title: format!("第{i}章"), goal: String::new(), beats: vec![] },
            )
            .unwrap();
            db.save_chapter_content(&novel.id, i, &format!("第{i}章正文"), "done").unwrap();
        }
        db.absorb_foreshadowings(
            &novel.id,
            1,
            &serde_json::json!({"foreshadowings_planted": [{"title": "甲伏笔"}]}),
        )
        .unwrap();
        db.save_global_review(
            &novel.id,
            &GlobalReview {
                ok: true,
                issues: vec![GlobalIssue {
                    severity: "low".into(),
                    note: "第1章节奏".into(),
                    affected_chapters: vec![1],
                    suggestion: String::new(),
                    status: "open".into(),
                    is_new: false,
                }],
                updated_at: "t".into(),
            },
        )
        .unwrap();
        // 交换 1/2：引用应跟随章节互换
        db.move_chapter(&novel.id, 1, 1).unwrap();
        let fs = db.list_foreshadowings(&novel.id).unwrap();
        assert_eq!(fs[0].planted_chapter, 2, "埋设章应跟随章节移动到 2");
        let gr = db.get_global_review(&novel.id).unwrap().unwrap();
        assert_eq!(gr.issues[0].affected_chapters, vec![2], "涉及章节应跟随章节移动");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn global_review_roundtrip_and_merge() {
        use crate::models::{GlobalIssue, GlobalReview};
        let dir = tmp_dir("global");
        std::fs::create_dir_all(&dir).unwrap();
        let db = Db::open(&dir.join("ainovel.db")).unwrap();
        let novel = db.create_novel("全局审校存储测试", "auto", "").unwrap();
        assert!(db.get_global_review(&novel.id).unwrap().is_none());
        let review = GlobalReview {
            ok: true,
            issues: vec![GlobalIssue {
                severity: "medium".into(),
                note: "第 4~7 章连续平淡，缺少冲突升级".into(),
                affected_chapters: vec![4, 5, 6, 7],
                suggestion: "在第 6 章加入一次试探性对抗".into(),
                status: "open".into(),
                is_new: true,
            }],
            updated_at: "2026-10-17T00:00:00+00:00".into(),
        };
        db.save_global_review(&novel.id, &review).unwrap();
        let loaded = db.get_global_review(&novel.id).unwrap().unwrap();
        assert_eq!(loaded.issues.len(), 1);
        assert_eq!(loaded.issues[0].affected_chapters, vec![4, 5, 6, 7]);
        assert_eq!(loaded.updated_at, "2026-10-17T00:00:00+00:00");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
