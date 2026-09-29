//! SQLite 持久化（sqlx，内置 libsqlite3，无需系统依赖）。
//!
//! 简单平铺的迁移：启动时执行 embedded schema.sql，后续需要演进时再引入
//! 正式的 migration 机制。

use anyhow::Result;
use serde::{Deserialize, Serialize};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePool, SqlitePoolOptions};
use std::path::Path;
use std::str::FromStr;

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS videos (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    number TEXT NOT NULL UNIQUE,
    title TEXT,
    meta_json TEXT NOT NULL,
    -- 1 = 这条是用户在多源结果里**人工精选**出来的。
    -- 人工结果优先级最高：process_task 见到它就不再刮削（源站挂了也能出片）。
    manual INTEGER NOT NULL DEFAULT 0,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at TEXT NOT NULL DEFAULT (datetime('now'))
);
CREATE TABLE IF NOT EXISTS tasks (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    kind TEXT NOT NULL DEFAULT 'scrape',
    status TEXT NOT NULL DEFAULT 'pending',
    source_path TEXT NOT NULL,
    dest_path TEXT,
    number TEXT,
    error TEXT,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at TEXT NOT NULL DEFAULT (datetime('now'))
);
"#;

pub async fn init_pool(db_path: &Path) -> Result<SqlitePool> {
    if let Some(parent) = db_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let opts = SqliteConnectOptions::from_str(&format!(
        "sqlite://{}",
        db_path.display()
    ))?
    .create_if_missing(true)
    .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal)
    .foreign_keys(true);

    let pool = SqlitePoolOptions::new().max_connections(8).connect_with(opts).await?;
    sqlx::raw_sql(SCHEMA).execute(&pool).await?;
    // 平铺迁移：给老库补列。列已存在时 ALTER 会报错，忽略即可。
    // （等结构再复杂一点再引正式的 migration 机制。）
    if let Err(e) =
        sqlx::raw_sql("ALTER TABLE videos ADD COLUMN manual INTEGER NOT NULL DEFAULT 0")
            .execute(&pool)
            .await
    {
        tracing::debug!(error = %e, "videos.manual 列已存在（正常，忽略）");
    }
    Ok(pool)
}

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct TaskRow {
    pub id: i64,
    pub kind: String,
    pub status: String,
    pub source_path: String,
    pub dest_path: Option<String>,
    pub number: Option<String>,
    pub error: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

pub async fn insert_task(pool: &SqlitePool, source_path: &str, number: Option<&str>) -> Result<i64> {
    let r = sqlx::query("INSERT INTO tasks (source_path, number) VALUES (?, ?)")
        .bind(source_path)
        .bind(number)
        .execute(pool)
        .await?;
    Ok(r.last_insert_rowid())
}

pub async fn update_task(
    pool: &SqlitePool,
    id: i64,
    status: &str,
    dest_path: Option<&str>,
    error: Option<&str>,
) -> Result<()> {
    sqlx::query(
        "UPDATE tasks SET status = ?, dest_path = COALESCE(?, dest_path), \
         error = ?, updated_at = datetime('now') WHERE id = ?",
    )
    .bind(status)
    .bind(dest_path)
    .bind(error)
    .bind(id)
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn list_tasks(pool: &SqlitePool) -> Result<Vec<TaskRow>> {
    let rows = sqlx::query_as::<_, TaskRow>(
        "SELECT * FROM tasks ORDER BY id DESC LIMIT 500",
    )
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

pub async fn upsert_video(
    pool: &SqlitePool,
    number: &str,
    title: Option<&str>,
    meta_json: &str,
) -> Result<()> {
    // ⚠️ `WHERE videos.manual = 0`：**人工精选的结果不许被自动刮削覆盖**。
    // 少了这个条件会出现「标记说人工、内容却是自动刮的」这种自相矛盾的状态。
    sqlx::query(
        "INSERT INTO videos (number, title, meta_json) VALUES (?, ?, ?) \
         ON CONFLICT(number) DO UPDATE SET title = excluded.title, \
         meta_json = excluded.meta_json, updated_at = datetime('now') \
         WHERE videos.manual = 0",
    )
    .bind(number)
    .bind(title)
    .bind(meta_json)
    .execute(pool)
    .await?;
    Ok(())
}

/// 保存「人工精选」的元数据（多源结果里用户挑的那一条）。
///
/// 人工结果的优先级最高：`process_task` 见到它就直接用、**不再刮削** ——
/// 所以源站全挂 / 番号谁都搜不到时，人工填一条也能出片。
pub async fn set_manual_meta(
    pool: &SqlitePool,
    number: &str,
    title: Option<&str>,
    meta_json: &str,
) -> Result<()> {
    sqlx::query(
        "INSERT INTO videos (number, title, meta_json, manual) VALUES (?, ?, ?, 1) \
         ON CONFLICT(number) DO UPDATE SET title = excluded.title, \
         meta_json = excluded.meta_json, manual = 1, updated_at = datetime('now')",
    )
    .bind(number)
    .bind(title)
    .bind(meta_json)
    .execute(pool)
    .await?;
    Ok(())
}

/// 取人工精选结果（只认 `manual = 1` 的）。
pub async fn get_manual_meta(pool: &SqlitePool, number: &str) -> Result<Option<String>> {
    let r: Option<(String,)> =
        sqlx::query_as("SELECT meta_json FROM videos WHERE number = ? AND manual = 1")
            .bind(number)
            .fetch_optional(pool)
            .await?;
    Ok(r.map(|t| t.0))
}

/// 取消人工精选：只把标记清掉，普通记录保留（下次会自动刮削）。
pub async fn clear_manual_meta(pool: &SqlitePool, number: &str) -> Result<u64> {
    let r = sqlx::query("UPDATE videos SET manual = 0, updated_at = datetime('now') WHERE number = ?")
        .bind(number)
        .execute(pool)
        .await?;
    Ok(r.rows_affected())
}

/// 列出所有人工精选（UI 展示用）。
pub async fn list_manual(pool: &SqlitePool) -> Result<Vec<(String, Option<String>)>> {
    let rows: Vec<(String, Option<String>)> = sqlx::query_as(
        "SELECT number, title FROM videos WHERE manual = 1 ORDER BY updated_at DESC LIMIT 500",
    )
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

pub async fn get_video_meta_json(pool: &SqlitePool, number: &str) -> Result<Option<String>> {
    let r: Option<(String,)> = sqlx::query_as("SELECT meta_json FROM videos WHERE number = ?")
        .bind(number)
        .fetch_optional(pool)
        .await?;
    Ok(r.map(|t| t.0))
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn pool(tag: &str) -> SqlitePool {
        let dir = std::env::temp_dir().join(format!("mdc_db_test_{tag}_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        init_pool(&dir.join("t.db")).await.unwrap()
    }

    #[tokio::test]
    async fn manual_meta_round_trip() {
        let p = pool("manual").await;
        assert!(get_manual_meta(&p, "MIDV-567").await.unwrap().is_none());

        set_manual_meta(&p, "MIDV-567", Some("人工挑的标题"), r#"{"number":"MIDV-567"}"#)
            .await
            .unwrap();
        assert_eq!(
            get_manual_meta(&p, "MIDV-567").await.unwrap().as_deref(),
            Some(r#"{"number":"MIDV-567"}"#)
        );
        assert_eq!(
            list_manual(&p).await.unwrap(),
            vec![("MIDV-567".to_string(), Some("人工挑的标题".to_string()))]
        );

        // 取消后不再算人工，但普通记录还在
        assert_eq!(clear_manual_meta(&p, "MIDV-567").await.unwrap(), 1);
        assert!(get_manual_meta(&p, "MIDV-567").await.unwrap().is_none());
        assert!(get_video_meta_json(&p, "MIDV-567").await.unwrap().is_some());
        assert!(list_manual(&p).await.unwrap().is_empty());
    }

    /// 🔴 自动刮削**不许覆盖**人工精选的结果。
    /// 少了这个约束会出现「标记说人工、内容却是自动刮的」这种自相矛盾的状态。
    #[tokio::test]
    async fn auto_scrape_does_not_overwrite_manual() {
        let p = pool("guard").await;
        set_manual_meta(&p, "MIDV-567", Some("人工"), r#"{"marker":"manual"}"#)
            .await
            .unwrap();

        upsert_video(&p, "MIDV-567", Some("自动"), r#"{"marker":"auto"}"#)
            .await
            .unwrap();

        assert_eq!(
            get_manual_meta(&p, "MIDV-567").await.unwrap().as_deref(),
            Some(r#"{"marker":"manual"}"#),
            "人工结果被自动刮削覆盖了"
        );

        // 非人工的记录照常可写
        upsert_video(&p, "SSIS-424", Some("自动"), r#"{"marker":"auto"}"#)
            .await
            .unwrap();
        assert_eq!(
            get_video_meta_json(&p, "SSIS-424").await.unwrap().as_deref(),
            Some(r#"{"marker":"auto"}"#)
        );
    }

    /// 老库（没有 manual 列）要能被 `init_pool` 的平铺迁移补上。
    #[tokio::test]
    async fn migration_adds_manual_column_to_old_db() {
        let dir = std::env::temp_dir().join(format!("mdc_db_old_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("old.db");
        {
            let opts = SqliteConnectOptions::from_str(&format!("sqlite://{}", path.display()))
                .unwrap()
                .create_if_missing(true);
            let old = SqlitePoolOptions::new().connect_with(opts).await.unwrap();
            // 模拟旧 schema：没有 manual 列
            sqlx::raw_sql(
                "CREATE TABLE videos (id INTEGER PRIMARY KEY AUTOINCREMENT, number TEXT NOT NULL UNIQUE, \
                 title TEXT, meta_json TEXT NOT NULL, \
                 created_at TEXT NOT NULL DEFAULT (datetime('now')), \
                 updated_at TEXT NOT NULL DEFAULT (datetime('now')));",
            )
            .execute(&old)
            .await
            .unwrap();
            old.close().await;
        }
        // 再走一次 init_pool，应当把 manual 列补上且不报错
        let p = init_pool(&path).await.unwrap();
        set_manual_meta(&p, "MIDV-567", None, r#"{"a":1}"#).await.unwrap();
        assert!(get_manual_meta(&p, "MIDV-567").await.unwrap().is_some());
    }
}
