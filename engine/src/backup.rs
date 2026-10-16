//! 数据备份 / 恢复。
//!
//! 备份包 = zip：
//! - `meta.json`：应用标识、引擎版本、生成时间
//! - `ainovel.db`：通过 `VACUUM INTO` 生成的一致性快照（含全部小说、台账、用量）
//!
//! 恢复 = 校验 zip → 解出 db → SQLite backup API 覆盖当前库（进程内完成）。

use std::io::Write;
use std::path::{Path, PathBuf};

pub const BACKUP_DB_ENTRY: &str = "ainovel.db";
pub const BACKUP_META_ENTRY: &str = "meta.json";

/// 生成备份 zip，返回文件字节数。
pub fn build_backup_zip(db: &crate::db::Db, out: &Path) -> anyhow::Result<u64> {
    let snapshot = out.with_extension("snapshot");
    db.vacuum_into(&snapshot)?;
    let result = (|| -> anyhow::Result<u64> {
        let file = std::fs::File::create(out)?;
        let mut zip = zip::ZipWriter::new(file);
        let opts = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);

        zip.start_file(BACKUP_META_ENTRY, opts)?;
        let meta = serde_json::json!({
            "app": "android-AI-Novel-Agent",
            "engine_version": env!("CARGO_PKG_VERSION"),
            "created_at": chrono::Utc::now().to_rfc3339(),
        });
        zip.write_all(meta.to_string().as_bytes())?;

        zip.start_file(BACKUP_DB_ENTRY, opts)?;
        let mut f = std::fs::File::open(&snapshot)?;
        std::io::copy(&mut f, &mut zip)?;
        zip.finish()?;
        Ok(std::fs::metadata(out)?.len())
    })();
    let _ = std::fs::remove_file(&snapshot);
    result
}

/// 校验备份 zip 并解出其中的数据库文件；返回临时 db 路径。
/// 校验点：包含 `ainovel.db`、能作为 SQLite 打开、存在 `novels` 表。
pub fn extract_backup_zip(zip_path: &Path, dest_dir: &Path) -> anyhow::Result<PathBuf> {
    let file = std::fs::File::open(zip_path)?;
    let mut archive = zip::ZipArchive::new(file)
        .map_err(|e| anyhow::anyhow!("不是有效的备份压缩包：{e}"))?;
    if archive.by_name(BACKUP_DB_ENTRY).is_err() {
        anyhow::bail!("备份文件缺少 {BACKUP_DB_ENTRY}");
    }
    let dest = dest_dir.join(format!("restore-{}.db", uuid::Uuid::new_v4()));
    {
        let mut entry = archive.by_name(BACKUP_DB_ENTRY)?;
        let mut out = std::fs::File::create(&dest)?;
        std::io::copy(&mut entry, &mut out)?;
    }
    let conn = rusqlite::Connection::open_with_flags(
        &dest,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .map_err(|e| anyhow::anyhow!("备份内的数据库无法打开：{e}"))?;
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='novels'",
        [],
        |r| r.get(0),
    )?;
    if count == 0 {
        anyhow::bail!("备份内容不是本应用的数据库（缺少 novels 表）");
    }
    Ok(dest)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::AppConfig;
    use crate::Engine;

    fn tmp_dir(tag: &str) -> std::path::PathBuf {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("ainovel-backup-{tag}-{stamp}"))
    }

    #[test]
    fn backup_and_restore_roundtrip() {
        let dir_a = tmp_dir("a");
        let engine_a = Engine::new(AppConfig::for_data_dir(dir_a.clone())).unwrap();
        let novel = engine_a.db.create_novel("备份测试书", "auto", "").unwrap();
        engine_a
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
        engine_a
            .db
            .save_chapter_content(&novel.id, 1, "正文内容", "done")
            .unwrap();

        let zip_path = dir_a.join("backup.zip");
        let size = build_backup_zip(&engine_a.db, &zip_path).unwrap();
        assert!(size > 0, "备份应产生内容");

        // 目标库：先放一本别的书，恢复后应只剩备份里的内容
        let dir_b = tmp_dir("b");
        let engine_b = Engine::new(AppConfig::for_data_dir(dir_b.clone())).unwrap();
        engine_b.db.create_novel("将被覆盖的书", "auto", "").unwrap();

        let db_path = extract_backup_zip(&zip_path, &dir_b).unwrap();
        engine_b.db.restore_from_file(&db_path).unwrap();

        let novels = engine_b.db.list_novels().unwrap();
        assert_eq!(novels.len(), 1, "恢复后应只剩备份中的书");
        assert_eq!(novels[0].novel.idea, "备份测试书");
        let ch = engine_b.db.get_chapter(&novels[0].novel.id, 1).unwrap().unwrap();
        assert_eq!(ch.content, "正文内容", "章节正文应随备份恢复");

        let _ = std::fs::remove_dir_all(&dir_a);
        let _ = std::fs::remove_dir_all(&dir_b);
    }

    #[test]
    fn restore_marks_stale_running_runs_failed() {
        // 「运行中」时打的备份：恢复后不能残留 running 的运行（否则再次创作会被 409 锁死）
        let dir_a = tmp_dir("stale-a");
        let engine_a = Engine::new(AppConfig::for_data_dir(dir_a.clone())).unwrap();
        let novel = engine_a.db.create_novel("运行中备份", "auto", "").unwrap();
        engine_a.db.create_run(&novel.id, 5).unwrap(); // 保持 running
        let zip_path = dir_a.join("backup.zip");
        build_backup_zip(&engine_a.db, &zip_path).unwrap();

        let dir_b = tmp_dir("stale-b");
        let engine_b = Engine::new(AppConfig::for_data_dir(dir_b.clone())).unwrap();
        let db_path = extract_backup_zip(&zip_path, &dir_b).unwrap();
        engine_b.db.restore_from_file(&db_path).unwrap();

        let run = engine_b.db.latest_run(&novel.id).unwrap().unwrap();
        assert_ne!(run.status, "running", "恢复后不应残留进行中的运行");
        assert_eq!(run.status, "failed");
        assert!(engine_b.db.any_running_run().unwrap().is_none());

        let _ = std::fs::remove_dir_all(&dir_a);
        let _ = std::fs::remove_dir_all(&dir_b);
    }

    #[test]
    fn rejects_invalid_backup() {
        let dir = tmp_dir("bad");
        std::fs::create_dir_all(&dir).unwrap();

        // 不是 zip
        let not_zip = dir.join("not-a-zip.zip");
        std::fs::write(&not_zip, b"hello world").unwrap();
        assert!(extract_backup_zip(&not_zip, &dir).is_err(), "非 zip 必须被拒绝");

        // 是 zip 但没有 ainovel.db
        let empty_zip = dir.join("empty.zip");
        {
            let file = std::fs::File::create(&empty_zip).unwrap();
            let mut zip = zip::ZipWriter::new(file);
            zip.start_file("other.txt", zip::write::SimpleFileOptions::default())
                .unwrap();
            zip.write_all(b"x").unwrap();
            zip.finish().unwrap();
        }
        assert!(extract_backup_zip(&empty_zip, &dir).is_err(), "缺库文件必须被拒绝");

        // 是 zip 且含 ainovel.db，但不是本应用的数据库
        let alien_zip = dir.join("alien.zip");
        let alien_db = dir.join("alien.db");
        {
            let conn = rusqlite::Connection::open(&alien_db).unwrap();
            conn.execute_batch("CREATE TABLE foo(id INTEGER);").unwrap();
            drop(conn);
            let file = std::fs::File::create(&alien_zip).unwrap();
            let mut zip = zip::ZipWriter::new(file);
            zip.start_file(BACKUP_DB_ENTRY, zip::write::SimpleFileOptions::default())
                .unwrap();
            let mut f = std::fs::File::open(&alien_db).unwrap();
            std::io::copy(&mut f, &mut zip).unwrap();
            zip.finish().unwrap();
        }
        assert!(
            extract_backup_zip(&alien_zip, &dir).is_err(),
            "缺少 novels 表的数据库必须被拒绝"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}
