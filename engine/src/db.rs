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

fn now() -> String {
    chrono::Utc::now().to_rfc3339()
}

const NOVEL_COLS: &str = "id,title,genre,idea,logline,protagonist,style,book_outline,status,mode,created_at,updated_at,style_preset";
const CHAPTER_COLS: &str = "id,novel_id,idx,title,goal,beats_json,content,summary,status,review_json,created_at,updated_at,style_report";
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
        // 旧库升级：补 chapters.summary 列
        Self::ensure_column(&conn, "chapters", "summary", "summary TEXT NOT NULL DEFAULT ''")?;
        // 章节结构编辑的中断兜底：把负区占位 idx 还原
        conn.execute_batch("UPDATE chapters SET idx = -idx - 1 WHERE idx < 0;")?;
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
            status: "created".into(),
            mode: if mode == "staged" { "staged".into() } else { "auto".into() },
            created_at: now(),
            updated_at: now(),
            style_preset: style_preset.trim().to_string(),
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
                chapters_done: row.get(13)?,
                words: row.get(14)?,
                run_status: row.get(15)?,
                run_stage: row.get(16)?,
                chapters_target: row.get(17)?,
                last_error: row.get(18)?,
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
        let n = conn.execute("DELETE FROM novels WHERE id=?1", [id])?;
        Ok(n > 0)
    }

    pub fn update_novel_brief(&self, id: &str, b: &Brief) -> anyhow::Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE novels SET title=?2, genre=?3, logline=?4, protagonist=?5, style=?6, updated_at=?7 WHERE id=?1",
            params![id, b.title, b.genre, b.logline, b.protagonist, b.style, now()],
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
        //          content(6) summary(7) status(8) review_json(9) created_at(10) updated_at(11) style_report(12)
        let beats_json: String = row.get(5)?;
        Ok(Chapter {
            id: row.get(0)?,
            novel_id: row.get(1)?,
            idx: row.get(2)?,
            title: row.get(3)?,
            goal: row.get(4)?,
            beats: serde_json::from_str(&beats_json).unwrap_or_default(),
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
        Ok(())
    }

    pub fn save_chapter_content(&self, novel_id: &str, idx: i64, content: &str, status: &str) -> anyhow::Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE chapters SET content=?3, status=?4, updated_at=?5 WHERE novel_id=?1 AND idx=?2",
            params![novel_id, idx, content, status, now()],
        )?;
        Ok(())
    }

    pub fn save_chapter_review(&self, novel_id: &str, idx: i64, review: &Review) -> anyhow::Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE chapters SET review_json=?3, updated_at=?4 WHERE novel_id=?1 AND idx=?2",
            params![novel_id, idx, serde_json::to_string(review)?, now()],
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
        conn.execute(
            "INSERT INTO runs(id, novel_id, status, stage, chapter_cursor, chapters_target, error, created_at, updated_at)
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9)",
            params![r.id, r.novel_id, r.status, r.stage, r.chapter_cursor, r.chapters_target, r.error, r.created_at, r.updated_at],
        )?;
        Ok(r)
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
        Ok(())
    }

    /// 编辑规划（仅限尚未开写的章节）
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
            "UPDATE chapters SET title=?3, goal=?4, beats_json=?5, updated_at=?6 WHERE novel_id=?1 AND idx=?2",
            params![novel_id, idx, title, goal, serde_json::to_string(beats)?, now()],
        )?;
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
        id: &str,
        role: &str,
        status: &str,
        facts: &[String],
    ) -> anyhow::Result<bool> {
        let conn = self.conn.lock().unwrap();
        let n = conn.execute(
            "UPDATE characters SET role=?2, status=?3, facts=?4, updated_at=?5 WHERE id=?1",
            params![id, role, status, serde_json::to_string(facts)?, now()],
        )?;
        Ok(n > 0)
    }

    pub fn delete_character(&self, id: &str) -> anyhow::Result<bool> {
        let conn = self.conn.lock().unwrap();
        Ok(conn.execute("DELETE FROM characters WHERE id=?1", [id])? > 0)
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
            let status = item["status"].as_str().unwrap_or("active").trim();
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
        id: &str,
        status: &str,
        notes: &str,
        resolved_chapter: Option<i64>,
    ) -> anyhow::Result<bool> {
        let conn = self.conn.lock().unwrap();
        let n = conn.execute(
            "UPDATE foreshadowings SET status=?2, notes=?3, resolved_chapter=?4, updated_at=?5 WHERE id=?1",
            params![id, status, notes, resolved_chapter, now()],
        )?;
        Ok(n > 0)
    }

    pub fn delete_foreshadowing(&self, id: &str) -> anyhow::Result<bool> {
        let conn = self.conn.lock().unwrap();
        Ok(conn.execute("DELETE FROM foreshadowings WHERE id=?1", [id])? > 0)
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
        let hit: Option<String> = conn
            .query_row(
                "SELECT id FROM foreshadowings
                 WHERE novel_id=?1 AND status='open'
                   AND (title=?2 OR title LIKE '%'||?2||'%' OR ?2 LIKE '%'||title||'%')
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
    /// 中断时由 migrate 的负区还原兜底。
    fn apply_chapter_order(
        conn: &Connection,
        novel_id: &str,
        order: &[(String, i64)],
    ) -> anyhow::Result<()> {
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
        Ok(())
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
        // 恢复后补跑轻量迁移（兼容更早版本的备份）
        self.migrate()?;
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
}
