//! roots / rules 的读写。

use rusqlite::{params, OptionalExtension};

use crate::error::{TdError, TdResult};
use crate::store::model::{rule_from_row, root_from_row, NewRule, Root, Rule, RULE_COLUMNS, ROOT_COLUMNS};
use crate::store::Store;
use crate::util;

/// SQLite `LIKE ... ESCAPE '\'` 的模式转义。
///
/// 注意 **反斜杠本身也必须转义**：搜索关键词 `Temp\Chrome` 里的 `\C`
/// 在 LIKE 里会被当成"转义后的 C"，结果是永远匹配不上任何以反斜杠开头的路径段。
/// 路径搜索是本工具最常见的操作，所以这条必须正确。
fn escape_like(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
}

impl Store {
    // ------------------------------------------------------------------
    // roots
    // ------------------------------------------------------------------
    pub fn list_roots(&self) -> TdResult<Vec<Root>> {
        let sql = format!("SELECT {ROOT_COLUMNS} FROM roots ORDER BY id");
        let mut stmt = self.conn().prepare(&sql)?;
        let rows = stmt.query_map([], root_from_row)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    pub fn get_root(&self, id: i64) -> TdResult<Root> {
        let sql = format!("SELECT {ROOT_COLUMNS} FROM roots WHERE id = ?1");
        self.conn()
            .query_row(&sql, [id], root_from_row)
            .optional()?
            .ok_or_else(|| TdError::NotFound(crate::i18n::tf("db.root_not_in_lib", &[("id", &id.to_string())])))
    }

    /// 按路径查找目标根（不存在则创建）。`path` 可为模板，入库前会规范化。
    pub fn ensure_root(&self, label: &str, path: &str, is_ramdisk: bool, wait_timeout_ms: i64) -> TdResult<i64> {
        let norm = util::normalize_root_path(path);
        if let Some(id) = self
            .conn()
            .query_row("SELECT id FROM roots WHERE path = ?1", [&norm], |r| {
                r.get::<_, i64>(0)
            })
            .optional()?
        {
            return Ok(id);
        }
        self.insert_root(label, path, is_ramdisk, wait_timeout_ms)
    }

    pub fn insert_root(&self, label: &str, path: &str, is_ramdisk: bool, wait_timeout_ms: i64) -> TdResult<i64> {
        let norm = util::normalize_root_path(path);
        if norm.trim().is_empty() {
            return Err(TdError::InvalidPath(crate::i18n::t("db.root_path_empty").into()));
        }
        self.conn().execute(
            "INSERT INTO roots(label, path, is_ramdisk, wait_timeout_ms) VALUES(?1, ?2, ?3, ?4)",
            params![label, norm, is_ramdisk, wait_timeout_ms],
        )?;
        Ok(self.conn().last_insert_rowid())
    }

    pub fn update_root(&self, id: i64, label: &str, path: &str, is_ramdisk: bool, wait_timeout_ms: i64) -> TdResult<()> {
        let norm = util::normalize_root_path(path);
        let n = self.conn().execute(
            "UPDATE roots SET label=?2, path=?3, is_ramdisk=?4, wait_timeout_ms=?5 WHERE id=?1",
            params![id, label, norm, is_ramdisk, wait_timeout_ms],
        )?;
        if n == 0 {
            return Err(TdError::NotFound(crate::i18n::tf("db.root_not_found", &[("id", &id.to_string())])));
        }
        Ok(())
    }

    pub fn delete_root(&self, id: i64) -> TdResult<()> {
        let used: i64 = self
            .conn()
            .query_row("SELECT count(*) FROM rules WHERE target_root_id = ?1", [id], |r| r.get(0))?;
        if used > 0 {
            return Err(TdError::Other(format!(
                "还有 {used} 条规则在引用这个目标根，请先删除或改绑这些规则"
            )));
        }
        let n = self.conn().execute("DELETE FROM roots WHERE id = ?1", [id])?;
        if n == 0 {
            return Err(TdError::NotFound(crate::i18n::tf("db.root_not_found", &[("id", &id.to_string())])));
        }
        Ok(())
    }

    // ------------------------------------------------------------------
    // rules
    // ------------------------------------------------------------------

    pub fn list_rules(&self) -> TdResult<Vec<Rule>> {
        let sql = format!(
            "SELECT {RULE_COLUMNS} FROM rules ORDER BY category, sort_order, id"
        );
        let mut stmt = self.conn().prepare(&sql)?;
        let rows = stmt.query_map([], rule_from_row)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    /// 列表查询：按分类 / 是否开启 / 关键词（名称或源路径模糊匹配）过滤。
    pub fn query_rules(
        &self,
        category: Option<&str>,
        enabled_only: bool,
        keyword: Option<&str>,
    ) -> TdResult<Vec<Rule>> {
        let mut sql = format!("SELECT {RULE_COLUMNS} FROM rules WHERE 1=1");
        let mut args: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();

        if let Some(c) = category {
            sql.push_str(" AND category = ?");
            args.push(Box::new(c.to_string()));
        }
        if enabled_only {
            sql.push_str(" AND want_enabled = 1");
        }
        if let Some(k) = keyword {
            let like = format!("%{}%", escape_like(&k.to_lowercase()));
            sql.push_str(" AND (lower(name) LIKE ? ESCAPE '\\' OR lower(source_template) LIKE ? ESCAPE '\\' OR lower(target_subpath) LIKE ? ESCAPE '\\')");
            args.push(Box::new(like.clone()));
            args.push(Box::new(like.clone()));
            args.push(Box::new(like));
        }
        sql.push_str(" ORDER BY category, sort_order, id");

        let mut stmt = self.conn().prepare(&sql)?;
        let refs: Vec<&dyn rusqlite::ToSql> = args.iter().map(|b| b.as_ref()).collect();
        let rows = stmt.query_map(refs.as_slice(), rule_from_row)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    pub fn get_rule(&self, id: i64) -> TdResult<Rule> {
        let sql = format!("SELECT {RULE_COLUMNS} FROM rules WHERE id = ?1");
        self.conn()
            .query_row(&sql, [id], rule_from_row)
            .optional()?
            .ok_or_else(|| TdError::NotFound(crate::i18n::tf("db.rule_not_found", &[("id", &id.to_string())])))
    }

    pub fn find_rule_by_source(&self, source_norm: &str) -> TdResult<Option<Rule>> {
        let sql = format!("SELECT {RULE_COLUMNS} FROM rules WHERE source_norm = ?1");
        Ok(self
            .conn()
            .query_row(&sql, [source_norm], rule_from_row)
            .optional()?)
    }

    /// 新建规则。`source_norm` 由**展开后**的路径算出来，所以
    /// `%LOCALAPPDATA%\X` 与 `C:\Users\a\AppData\Local\X` 会被认为是同一条。
    pub fn insert_rule(&self, nr: &NewRule) -> TdResult<i64> {
        let (source, unknown) = util::expand_path(&nr.source_template);
        if !unknown.is_empty() {
            return Err(TdError::InvalidPath(crate::i18n::tf("db.unresolved_vars", &[("vars", &unknown.join("、"))])));
        }
        if !source.is_absolute() {
            return Err(TdError::InvalidPath(crate::i18n::t("db.must_be_absolute").into()));
        }
        let norm = util::normalize_key(&source.to_string_lossy());
        if self.find_rule_by_source(&norm)?.is_some() {
            return Err(TdError::Other(crate::i18n::tf("db.duplicate_rule", &[("norm", &norm)])));
        }
        let pf = crate::safety::preflight_source(&source);
        if pf.is_blocked() {
            return Err(TdError::SafetyBlocked(pf.blocking.join("；")));
        }
        let needs_elev = pf.requires_elevation;

        self.conn().execute(
            "INSERT INTO rules(name, category, source_template, source_norm, target_root_id,
                               target_subpath, link_type, want_enabled, rebuild_on_boot,
                               notes, needs_elevation, excludes, auto_clean_policy)
             VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
            params![
                nr.name,
                nr.category,
                nr.source_template,
                norm,
                nr.target_root_id,
                nr.target_subpath,
                nr.link_type,
                nr.want_enabled,
                nr.rebuild_on_boot,
                nr.notes,
                needs_elev,
                serde_json::to_string(&nr.excludes).unwrap_or_else(|_| "[]".into()),
                nr.auto_clean_policy,
            ],
        )?;
        Ok(self.conn().last_insert_rowid())
    }

    pub fn update_rule(&self, id: i64, nr: &NewRule) -> TdResult<()> {
        let (source, unknown) = util::expand_path(&nr.source_template);
        if !unknown.is_empty() {
            return Err(TdError::InvalidPath(crate::i18n::tf("db.unresolved_vars", &[("vars", &unknown.join("、"))])));
        }
        let norm = util::normalize_key(&source.to_string_lossy());
        if let Some(other) = self.find_rule_by_source(&norm)? {
            if other.id != id {
                return Err(TdError::Other(crate::i18n::tf("db.duplicate_rule_other", &[("id", &other.id.to_string())])));
            }
        }

        let n = self.conn().execute(
            "UPDATE rules SET name=?2, category=?3, source_template=?4, source_norm=?5,
                    target_root_id=?6, target_subpath=?7, link_type=?8,
                    want_enabled=?9, rebuild_on_boot=?10, notes=?11, excludes=?12,
                    auto_clean_policy=?13, updated_at=datetime('now')
             WHERE id=?1",
            params![
                id,
                nr.name,
                nr.category,
                nr.source_template,
                norm,
                nr.target_root_id,
                nr.target_subpath,
                nr.link_type,
                nr.want_enabled,
                nr.rebuild_on_boot,
                nr.notes,
                serde_json::to_string(&nr.excludes).unwrap_or_else(|_| "[]".into()),
                nr.auto_clean_policy,
            ],
        )?;
        if n == 0 {
            return Err(TdError::NotFound(crate::i18n::tf("db.rule_not_found", &[("id", &id.to_string())])));
        }
        Ok(())
    }

    pub fn delete_rule(&self, id: i64) -> TdResult<()> {
        let n = self.conn().execute("DELETE FROM rules WHERE id = ?1", [id])?;
        if n == 0 {
            return Err(TdError::NotFound(crate::i18n::tf("db.rule_not_found", &[("id", &id.to_string())])));
        }
        Ok(())
    }

    pub fn set_rule_enabled(&self, id: i64, enabled: bool) -> TdResult<()> {
        let n = self.conn().execute(
            "UPDATE rules SET want_enabled=?2, updated_at=datetime('now') WHERE id=?1",
            params![id, enabled],
        )?;
        if n == 0 {
            return Err(TdError::NotFound(crate::i18n::tf("db.rule_not_found", &[("id", &id.to_string())])));
        }
        Ok(())
    }

    /// 按传入顺序重写这些规则的 `sort_order`（1 起）。只动列表里的规则，其余不动。
    /// 多条 UPDATE 包在一个事务里：要么整批生效，要么整批不变 ——
    /// 拖拽排序要么成功要么维持原样，不能出现"一半规则换了位置"的中间态。
    /// id 的合法性 / 同类 / 无重复 / 覆盖完整分类，由 usecase 层（reorder_rules）把关。
    pub fn set_rules_order(&self, ordered_ids: &[i64]) -> TdResult<usize> {
        let conn = self.conn();
        let tx = conn.unchecked_transaction()?;
        for (i, id) in ordered_ids.iter().enumerate() {
            let n = tx.execute(
                "UPDATE rules SET sort_order=?2 WHERE id=?1",
                params![id, (i + 1) as i64],
            )?;
            if n == 0 {
                return Err(TdError::NotFound(crate::i18n::tf("db.rule_not_found", &[("id", &id.to_string())])));
            }
        }
        tx.commit()?;
        Ok(ordered_ids.len())
    }

    pub fn set_rule_flags(
        &self,
        id: i64,
        rebuild_on_boot: bool,
        link_type: &str,
        acl_sddl: Option<&str>,
    ) -> TdResult<()> {
        self.conn().execute(
            "UPDATE rules SET rebuild_on_boot=?2, link_type=?3, acl_sddl=?4,
                    updated_at=datetime('now')
             WHERE id=?1",
            params![id, rebuild_on_boot, link_type, acl_sddl],
        )?;
        Ok(())
    }

    /// 写入检测结果缓存。
    pub fn set_rule_state(&self, id: i64, state: &str, error: Option<&str>) -> TdResult<()> {
        self.conn().execute(
            "UPDATE rules SET last_state=?2, last_error=?3, last_checked_at=datetime('now') WHERE id=?1",
            params![id, state, error],
        )?;
        Ok(())
    }

    /// 记录一次成功应用后的规模信息。
    pub fn set_rule_applied(&self, id: i64, files: i64, bytes: i64) -> TdResult<()> {
        self.conn().execute(
            "UPDATE rules SET last_files=?2, last_bytes=?3, applied_at=datetime('now'),
                    last_state='link_ok', last_error=NULL WHERE id=?1",
            params![id, files, bytes],
        )?;
        Ok(())
    }

    /// 开机重建的处理对象：开启 + 需要重建。
    pub fn rules_for_boot(&self) -> TdResult<Vec<Rule>> {
        let sql = format!(
            "SELECT {RULE_COLUMNS} FROM rules
             WHERE want_enabled = 1 AND rebuild_on_boot = 1
             ORDER BY target_subpath, id"
        );
        let mut stmt = self.conn().prepare(&sql)?;
        let rows = stmt.query_map([], rule_from_row)?;
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
    use crate::store::Store;

    fn mk_store() -> Store {
        let s = Store::open_in_memory().unwrap();
        s.insert_root("RAMDisk", "R:", true, 90000).unwrap();
        s
    }

    fn new_rule(name: &str, src: &str, sub: &str) -> NewRule {
        NewRule {
            name: name.into(),
            category: "browser".into(),
            source_template: src.into(),
            target_root_id: 1,
            target_subpath: sub.into(),
            link_type: "junction".into(),
            want_enabled: true,
            rebuild_on_boot: true,
            notes: None,
            excludes: Vec::new(),
            auto_clean_policy: String::new(),
        }
    }

    #[test]
    fn root_crud() {
        let s = mk_store();
        assert_eq!(s.list_roots().unwrap().len(), 1);
        let id = s.ensure_root("RAMDisk", "R:", true, 90000).unwrap();
        assert_eq!(id, 1, "相同路径应复用而不是新建");
        let id2 = s.insert_root("Second", "S:\\Redir", true, 30000).unwrap();
        assert_eq!(s.list_roots().unwrap().len(), 2);
        s.update_root(id2, "Second2", "S:\\Redir", true, 45000).unwrap();
        assert_eq!(s.get_root(id2).unwrap().wait_timeout_ms, 45000);
        s.delete_root(id2).unwrap();
        assert_eq!(s.list_roots().unwrap().len(), 1);
    }

    #[test]
    fn delete_root_blocked_when_referenced() {
        let s = mk_store();
        s.insert_rule(&new_rule("Chrome", r"C:\Users\a\AppData\Local\Chrome\Cache", "browsers\\chrome"))
            .unwrap();
        assert!(s.delete_root(1).is_err());
    }

    #[test]
    fn rule_insert_dedupes_by_expanded_path() {
        let s = mk_store();
        let id = s
            .insert_rule(&new_rule("A", r"C:\Temp\Cache", "a"))
            .unwrap();
        assert!(id > 0);
        // 大小写不同 / 尾分隔符不同 → 视为同一条
        let dup = s.insert_rule(&new_rule("B", r"c:\temp\cache\", "b"));
        assert!(dup.is_err(), "重复路径应被拒绝");
    }

    #[test]
    fn rule_rejects_system_dir() {
        let s = mk_store();
        let bad = s.insert_rule(&new_rule("bad", r"C:\Windows\System32", "x"));
        assert!(matches!(bad, Err(TdError::SafetyBlocked(_))), "{bad:?}");
    }

    #[test]
    fn rule_needs_elevation_flag() {
        let s = mk_store();
        let id = s
            .insert_rule(&new_rule("pf", r"C:\Program Files\Foo\Cache", "pf"))
            .unwrap();
        assert!(s.get_rule(id).unwrap().needs_elevation);
    }

    #[test]
    fn query_filters_work() {
        let s = mk_store();
        s.insert_rule(&new_rule("Chrome", r"C:\Temp\Chrome", "c")).unwrap();
        s.insert_rule(&new_rule("Photoshop", r"C:\Temp\PS", "p")).unwrap();
        s.set_rule_enabled(2, false).unwrap();

        assert_eq!(s.query_rules(None, false, None).unwrap().len(), 2);
        assert_eq!(s.query_rules(None, true, None).unwrap().len(), 1);
        assert_eq!(s.query_rules(Some("browser"), false, None).unwrap().len(), 2);
        assert_eq!(s.query_rules(None, false, Some("photoshop")).unwrap().len(), 1);
        assert_eq!(s.query_rules(None, false, Some("Temp\\Chrome")).unwrap().len(), 1);
        assert_eq!(s.query_rules(None, false, Some("不存在")).unwrap().len(), 0);
    }

    #[test]
    fn resolved_target_joins_subpath() {
        let s = mk_store();
        let id = s.insert_rule(&new_rule("A", r"C:\Temp\A", "browsers\\chrome")).unwrap();
        let rule = s.get_rule(id).unwrap();
        let root = s.get_root(1).unwrap();
        let (t, unk) = rule.resolved_target(&root);
        assert!(unk.is_empty());
        assert_eq!(t.to_string_lossy().to_lowercase(), "r:\\browsers\\chrome");
    }

    #[test]
    fn rules_for_boot_only_enabled() {
        let s = mk_store();
        s.insert_rule(&new_rule("A", r"C:\Temp\A", "a")).unwrap();
        s.insert_rule(&new_rule("B", r"C:\Temp\B", "b")).unwrap();
        s.set_rule_enabled(1, false).unwrap();
        let boot = s.rules_for_boot().unwrap();
        assert_eq!(boot.len(), 1);
        assert_eq!(boot[0].name, "B");
    }

    #[test]
    fn excludes_roundtrip() {
        let s = mk_store();
        let mut nr = new_rule("EX", r"C:\Temp\EX", "ex");
        nr.excludes = vec!["sessions".into(), "logs/old".into()];
        let id = s.insert_rule(&nr).unwrap();
        assert_eq!(
            s.get_rule(id).unwrap().excludes,
            vec!["sessions".to_string(), "logs/old".to_string()]
        );

        // 更新为空 → 覆盖
        let cleared = new_rule("EX", r"C:\Temp\EX", "ex2");
        s.update_rule(id, &cleared).unwrap();
        assert!(s.get_rule(id).unwrap().excludes.is_empty());

        // 默认无排除
        let id2 = s.insert_rule(&new_rule("D", r"C:\Temp\D", "d")).unwrap();
        assert!(s.get_rule(id2).unwrap().excludes.is_empty());
    }

    #[test]
    fn bad_excludes_json_falls_back_to_empty() {
        let s = mk_store();
        let id = s.insert_rule(&new_rule("BAD", r"C:\Temp\BAD", "bad")).unwrap();
        // 模拟手工改库写坏 JSON
        s.conn()
            .execute("UPDATE rules SET excludes='{oops' WHERE id=?1", [id])
            .unwrap();
        assert!(s.get_rule(id).unwrap().excludes.is_empty());
    }

    #[test]
    fn auto_clean_policy_roundtrip() {
        let s = mk_store();
        let mut nr = new_rule("AC", r"C:\Temp\AC", "ac");
        nr.auto_clean_policy = "older:3600".into();
        let id = s.insert_rule(&nr).unwrap();
        assert_eq!(s.get_rule(id).unwrap().auto_clean_policy, "older:3600");

        // 更新覆盖 & 默认未启用
        let off = new_rule("AC", r"C:\Temp\AC", "ac2");
        s.update_rule(id, &off).unwrap();
        assert_eq!(s.get_rule(id).unwrap().auto_clean_policy, "");
        let id2 = s.insert_rule(&new_rule("N", r"C:\Temp\N", "n")).unwrap();
        assert_eq!(s.get_rule(id2).unwrap().auto_clean_policy, "");
    }

    #[test]
    fn v2_database_migrates_to_v3() {
        // 手工造一个 v2 库（有 excludes 无 auto_clean_policy），迁移后补列且数据无损。
        let dir = std::env::temp_dir().join(format!("td_v3mig_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let db = dir.join("mig.db");
        {
            let conn = rusqlite::Connection::open(&db).unwrap();
            conn.execute_batch(
                "CREATE TABLE rules(id INTEGER PRIMARY KEY, name TEXT NOT NULL,
                    category TEXT NOT NULL, source_template TEXT NOT NULL,
                    source_norm TEXT NOT NULL UNIQUE, target_root_id INTEGER NOT NULL,
                    target_subpath TEXT NOT NULL, link_type TEXT NOT NULL DEFAULT 'junction',
                    want_enabled INTEGER NOT NULL DEFAULT 0,
                    rebuild_on_boot INTEGER NOT NULL DEFAULT 1,
                    acl_sddl TEXT, needs_elevation INTEGER NOT NULL DEFAULT 0,
                    notes TEXT, sort_order INTEGER NOT NULL DEFAULT 0,
                    excludes TEXT NOT NULL DEFAULT '[]',
                    last_state TEXT, last_checked_at TEXT, last_error TEXT,
                    last_files INTEGER, last_bytes INTEGER, applied_at TEXT,
                    created_at TEXT NOT NULL DEFAULT (datetime('now')),
                    updated_at TEXT NOT NULL DEFAULT (datetime('now')));
                 PRAGMA user_version=2;
                 INSERT INTO rules(name, category, source_template, source_norm,
                     target_root_id, target_subpath, excludes)
                 VALUES('M', 'dev', 'C:\\Temp\\M', 'c:\\temp\\m', 1, 'm', '[\"x\"]');",
            )
            .unwrap();
        }
        let s = Store::open(&db).unwrap();
        assert_eq!(s.user_version().unwrap(), crate::store::SCHEMA_VERSION);
        let r = s.get_rule(1).unwrap();
        assert_eq!(r.name, "M");
        assert_eq!(r.excludes, vec!["x".to_string()], "v2 数据迁移后无损");
        assert_eq!(r.auto_clean_policy, "", "v3 新列默认未启用");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
