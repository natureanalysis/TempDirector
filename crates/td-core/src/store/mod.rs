//! 规则库（SQLite）。
//!
//! ## 为什么放 `%PROGRAMDATA%` 而不是 `%APPDATA%`
//!
//! 后台服务以 SYSTEM 身份跑，读不到某个用户的 `%APPDATA%`。规则是全局共享的，
//! 而路径里的 `%APPDATA%` 这类变量在**执行时按当前用户展开**，所以一份库够用。
//!
//! ## 并发
//!
//! GUI 与服务可能同时打开这个库，所以固定开 WAL + `busy_timeout`，
//! 并且**不持有长事务**：所有写操作都是短事务。

pub mod model;
pub mod ops;
pub mod rules;

pub use model::{
    category_label, EventRow, NewRule, Operation, Root, Rule, CATEGORIES, RULE_COLUMNS,
    ROOT_COLUMNS,
};

use std::path::Path;
use std::time::Duration;

use rusqlite::Connection;

use crate::error::TdResult;

/// 当前 schema 版本。加迁移时递增，并在 `migrate` 里补一段 `if current < N`。
pub const SCHEMA_VERSION: i32 = 2;

/// 视为"已结束"的操作流水阶段。其余阶段在启动时都算待修复项。
pub const OP_TERMINAL_PHASES: [&str; 3] = ["done", "failed", "rolled_back"];

pub struct Store {
    conn: Connection,
}

impl Store {
    pub fn open(path: &Path) -> TdResult<Self> {
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)?;
            }
        }
        Self::new(Connection::open(path)?)
    }

    /// 打开默认位置（`%PROGRAMDATA%\TempDirector\tempdirector.db`）。
    pub fn open_default() -> TdResult<Self> {
        crate::paths::ensure_dirs()?;
        Self::open(&crate::paths::db_path())
    }

    /// 内存库，供单元测试使用。
    pub fn open_in_memory() -> TdResult<Self> {
        Self::new(Connection::open_in_memory()?)
    }

    fn new(conn: Connection) -> TdResult<Self> {
        // 内存库不支持 WAL，因此这三条都允许失败。
        let _ = conn.query_row("PRAGMA journal_mode=WAL", [], |r| r.get::<_, String>(0));
        let _ = conn.busy_timeout(Duration::from_millis(5000));
        let _ = conn.execute("PRAGMA foreign_keys=ON", []);
        let _ = conn.execute("PRAGMA synchronous=NORMAL", []);

        let mut store = Store { conn };
        store.migrate()?;
        Ok(store)
    }

    pub fn conn(&self) -> &Connection {
        &self.conn
    }

    pub fn user_version(&self) -> TdResult<i32> {
        Ok(self.conn.query_row("PRAGMA user_version", [], |r| r.get(0))?)
    }

    /// 当前本地时间（`YYYY-MM-DD HH:MM:SS`）。用于给人看的"上次做了某事"。
    pub fn now(&self) -> TdResult<String> {
        Ok(self
            .conn
            .query_row("SELECT datetime('now', 'localtime')", [], |r| r.get(0))?)
    }

    fn migrate(&mut self) -> TdResult<()> {
        let current = self.user_version()?;
        if current < 1 {
            self.conn.execute_batch(include_str!("schema.sql"))?;
        } else if current == 1 {
            // v2：规则加清理排除模式（JSON 数组）。schema.sql 已含该列，
            // 这里只对 v1 老库做增量（新库走上面的建表，不能再 ALTER 一次）。
            self.conn
                .execute_batch("ALTER TABLE rules ADD COLUMN excludes TEXT NOT NULL DEFAULT '[]';")?;
        }
        self.conn
            .pragma_update(None, "user_version", SCHEMA_VERSION)?;
        Ok(())
    }

    // ---------------- settings ----------------

    pub fn get_setting(&self, key: &str) -> TdResult<Option<String>> {
        let mut stmt = self
            .conn
            .prepare("SELECT v FROM settings WHERE k = ?1")?;
        let mut rows = stmt.query([key])?;
        match rows.next()? {
            Some(row) => Ok(Some(row.get(0)?)),
            None => Ok(None),
        }
    }

    pub fn set_setting(&self, key: &str, value: &str) -> TdResult<()> {
        self.conn.execute(
            "INSERT INTO settings(k, v) VALUES(?1, ?2)
             ON CONFLICT(k) DO UPDATE SET v = excluded.v",
            rusqlite::params![key, value],
        )?;
        Ok(())
    }

    pub fn all_settings(&self) -> TdResult<Vec<(String, String)>> {
        let mut stmt = self.conn.prepare("SELECT k, v FROM settings ORDER BY k")?;
        let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schema_migrates_to_current_version() {
        let mut store = Store::open_in_memory().unwrap();
        assert_eq!(store.user_version().unwrap(), SCHEMA_VERSION);
        // 再跑一次不应报错（幂等）
        store.migrate().unwrap();
        assert_eq!(store.user_version().unwrap(), SCHEMA_VERSION);
    }

    #[test]
    fn tables_exist() {
        let store = Store::open_in_memory().unwrap();
        for t in ["roots", "rules", "operations", "events", "settings"] {
            let n: i64 = store
                .conn()
                .query_row(
                    "SELECT count(*) FROM sqlite_master WHERE type='table' AND name=?1",
                    [t],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(n, 1, "缺少表 {t}");
        }
    }

    #[test]
    fn settings_roundtrip() {
        let store = Store::open_in_memory().unwrap();
        assert!(store.get_setting("k").unwrap().is_none());
        store.set_setting("k", "v1").unwrap();
        assert_eq!(store.get_setting("k").unwrap().as_deref(), Some("v1"));
        store.set_setting("k", "v2").unwrap();
        assert_eq!(store.get_setting("k").unwrap().as_deref(), Some("v2"));
    }
}
