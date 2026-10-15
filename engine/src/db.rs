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

fn now() -> String {
    chrono::Utc::now().to_rfc3339()
}

const NOVEL_COLS: &str = "id,title,genre,idea,logline,protagonist,style,book_outline,status,created_at,updated_at";
const CHAPTER_COLS: &str = "id,novel_id,idx,title,goal,beats_json,content,status,review_json,created_at,updated_at";
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
        Ok(db)
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
            CREATE INDEX IF NOT EXISTS idx_chapters_novel ON chapters(novel_id, idx);
            CREATE INDEX IF NOT EXISTS idx_runs_novel ON runs(novel_id, created_at);
            "#,
        )?;
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

    // ---------- novels ----------

    pub fn create_novel(&self, idea: &str) -> anyhow::Result<Novel> {
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
            created_at: now(),
            updated_at: now(),
        };
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO novels(id,title,genre,idea,logline,protagonist,style,book_outline,status,created_at,updated_at)
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)",
            params![n.id, n.title, n.genre, n.idea, n.logline, n.protagonist, n.style, n.book_outline, n.status, n.created_at, n.updated_at],
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
            created_at: row.get(9)?,
            updated_at: row.get(10)?,
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
                chapters_done: row.get(11)?,
                words: row.get(12)?,
                run_status: row.get(13)?,
                run_stage: row.get(14)?,
                chapters_target: row.get(15)?,
                last_error: row.get(16)?,
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
        //          content(6) status(7) review_json(8) created_at(9) updated_at(10)
        let beats_json: String = row.get(5)?;
        Ok(Chapter {
            id: row.get(0)?,
            novel_id: row.get(1)?,
            idx: row.get(2)?,
            title: row.get(3)?,
            goal: row.get(4)?,
            beats: serde_json::from_str(&beats_json).unwrap_or_default(),
            content: row.get(6)?,
            status: row.get(7)?,
            review_json: row.get(8)?,
            created_at: row.get(9)?,
            updated_at: row.get(10)?,
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
}
