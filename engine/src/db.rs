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

const NOVEL_COLS: &str = "id,title,genre,idea,logline,protagonist,style,book_outline,status,mode,created_at,updated_at";
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
            "#,
        )?;
        // 旧库升级：补 novels.mode 列
        Self::ensure_column(&conn, "novels", "mode", "mode TEXT NOT NULL DEFAULT 'auto'")?;
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

    // ---------- novels ----------

    pub fn create_novel(&self, idea: &str, mode: &str) -> anyhow::Result<Novel> {
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
        };
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO novels(id,title,genre,idea,logline,protagonist,style,book_outline,status,mode,created_at,updated_at)
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)",
            params![n.id, n.title, n.genre, n.idea, n.logline, n.protagonist, n.style, n.book_outline, n.status, n.mode, n.created_at, n.updated_at],
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
                chapters_done: row.get(12)?,
                words: row.get(13)?,
                run_status: row.get(14)?,
                run_stage: row.get(15)?,
                chapters_target: row.get(16)?,
                last_error: row.get(17)?,
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

    // ---------- 作品设置 ----------

    pub fn set_novel_mode(&self, id: &str, mode: &str) -> anyhow::Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE novels SET mode=?2, updated_at=?3 WHERE id=?1",
            params![id, mode, now()],
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
}
