//! 开机批量重建：让所有被重定向的缓存目录在软件启动之前就"存在且可写"。
//!
//! ## 时序
//!
//! 必须在目标软件的启动项之前跑完。软件是 `HKCU\Run` 拉起的，它的顺序不可控，
//! 所以正道是**后台服务监听登录事件**（`WTS_SESSION_LOGON`，比 Run 键早），
//! 用该用户的令牌执行本函数 —— 这样 `%APPDATA%` 展开正确、目录 ACL 也对。
//!
//! ## 三种重建动作
//!
//! | 情况 | 动作 |
//! |---|---|
//! | 链接有效，只是目标目录没了 | 建目录 → 链接自动恢复（**这是最常见的情况**） |
//! | 源完全不存在 | 父目录在的话直接把链接建出来，软件装好就能用 |
//! | 源变回了真实目录（软件更新覆盖） | **不自动动它**，报警告交给人处理 |
//!
//! 第三条是刻意的：真实目录里可能有新写入的缓存，自动转成链接意味着要先搬运，
//! 这属于"会动数据"的操作，不应该在开机静默路径里做。

use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::boot::acl;
use crate::engine::{self, rule_link_type};
use crate::error::TdResult;
use crate::fsops::mover::Progress;
use crate::i18n::{t, tf};
use crate::link::create::{create_dir_link, ensure_dir};
use crate::link::detect::{detect_link, LinkState};
use crate::store::{Root, Rule, Store};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RootOutcome {
    pub root_id: i64,
    pub label: String,
    pub path: String,
    pub ready: bool,
    pub waited_ms: u128,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct BootReport {
    pub roots: Vec<RootOutcome>,
    pub rules_total: usize,
    /// 新建出来的目标目录数（开机守护的核心产出）。
    pub dirs_created: usize,
    /// 因为目录被重建而自动恢复可用的链接数。
    pub links_recovered: usize,
    /// 链接本身丢了、由我们主动重建的链接数。
    pub links_repaired: usize,
    /// 本来就没问题、不需要任何动作的链接数。
    pub already_ok: usize,
    pub warnings: Vec<String>,
    pub failed: Vec<(i64, String)>,
    pub elapsed_ms: u128,
}

impl BootReport {
    pub fn ok_count(&self) -> usize {
        self.links_recovered + self.links_repaired + self.already_ok
    }

    pub fn has_problem(&self) -> bool {
        !self.warnings.is_empty() || !self.failed.is_empty() || self.roots.iter().any(|r| !r.ready)
    }

    pub fn one_line(&self) -> String {
        crate::i18n::tf("boot.one_line", &[
            ("ready", &self.roots.iter().filter(|r| r.ready).count().to_string()),
            ("total", &self.roots.len().to_string()),
            ("rules", &self.rules_total.to_string()),
            ("dirs", &self.dirs_created.to_string()),
            ("recovered", &self.links_recovered.to_string()),
            ("repaired", &self.links_repaired.to_string()),
            ("ok", &self.already_ok.to_string()),
            ("warn", &self.warnings.len().to_string()),
            ("failed", &self.failed.len().to_string()),
            ("ms", &crate::util::human_dur_ms(self.elapsed_ms)),
        ])
    }
}

enum RebuildAction {
    AlreadyPresent,
    Relinked,
    NeedsAttention(String),
}

/// 开机重建的分工范围。
///
/// 登录时注册了两个任务：普通权限任务处理普通源，最高权限任务只处理
/// `C:\Windows` / `Program Files` 这类受保护源。两条任务各写各的报告，
/// 界面把两份都读出来展示。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BootScope {
    /// 全部规则（手动执行 / CLI 默认）。
    All,
    /// 只处理需要管理员权限的源（高权限登录任务）。
    ElevatedOnly,
    /// 只处理普通源（普通登录任务）。
    NonElevatedOnly,
}

impl BootScope {
    pub fn parse(flag: &str) -> Option<Self> {
        match flag {
            "" | "all" => Some(Self::All),
            "elevated_only" | "elevated-only" => Some(Self::ElevatedOnly),
            "non_elevated_only" | "skip-elevated" => Some(Self::NonElevatedOnly),
            _ => None,
        }
    }

    fn accepts(self, elevated: bool) -> bool {
        match self {
            Self::All => true,
            Self::ElevatedOnly => elevated,
            Self::NonElevatedOnly => !elevated,
        }
    }

    fn report_keys(self) -> (&'static str, &'static str) {
        match self {
            Self::ElevatedOnly => ("last_boot_report_elevated", "last_boot_at_elevated"),
            _ => ("last_boot_report", "last_boot_at"),
        }
    }
}

/// 执行一次开机重建。
pub fn run_boot_rebuild(store: &Store) -> TdResult<BootReport> {
    run_boot_rebuild_scoped(store, BootScope::All)
}

/// 按权限分工执行开机重建。
pub fn run_boot_rebuild_scoped(store: &Store, scope: BootScope) -> TdResult<BootReport> {
    let started = Instant::now();
    let mut report = BootReport::default();

    // 高权限任务在非管理员上下文里跑（用户不在 Administrators 组）时，
    // 受保护目录的链接一个也建不出来 —— 提前说清楚，别让用户去猜。
    let elevated = crate::elevate::is_elevated();
    if scope == BootScope::ElevatedOnly && !elevated {
        report.warnings.push(t("boot.not_elevated").into());
    }

    // ---- 1. 等各目标卷就绪 ----
    let roots = store.list_roots()?;
    let mut ready_ids: Vec<i64> = Vec::new();

    for root in &roots {
        let (path, unknown) = root.resolved_path();
        let mut outcome = RootOutcome {
            root_id: root.id,
            label: root.label.clone(),
            path: path.display().to_string(),
            ready: false,
            waited_ms: 0,
            error: None,
        };

        if !unknown.is_empty() {
            let msg = tf("boot.bad_path", &[("vars", &unknown.join("、"))]);
            outcome.error = Some(msg.clone());
            let _ = store.log_event(None, "error", "root_bad_path", &msg);
            report.roots.push(outcome);
            continue;
        }

        let timeout = Duration::from_millis(root.wait_timeout_ms.max(1000) as u64);
        // wait_for_root（而不是 wait_for_volume）：目标根可能是 RAM 盘下的子目录，
        // RAM 盘重启后它自己也消失了 —— 必须"等卷 + 建目录"，不能等目录自己出现。
        match crate::boot::volume::wait_for_root(&path, timeout, Duration::from_millis(500)) {
            Ok(waited) => {
                outcome.ready = true;
                outcome.waited_ms = waited.as_millis();
                ready_ids.push(root.id);
                let _ = store.log_event(
                    None,
                    "info",
                    "root_ready",
                    &tf("boot.root_ready", &[
                        ("path", &path.display().to_string()),
                        ("secs", &format!("{:.1}", waited.as_secs_f64())),
                    ]),
                );
            }
            Err(e) => {
                outcome.error = Some(e.to_string());
                let _ = store.log_event(None, "error", "root_timeout", &e.to_string());
            }
        }
        report.roots.push(outcome);
    }

    // ---- 2. 收集待处理规则，按目标层级从浅到深排序 ----
    let mut pending: Vec<(Rule, Root)> = Vec::new();
    for rule in store.rules_for_boot()? {
        if !ready_ids.contains(&rule.target_root_id) {
            continue;
        }
        if scope != BootScope::All {
            // 解析失败（环境变量属于别的用户等）的规则交给普通任务去报告，
            // 免得两条任务各报一遍同样的错。
            let source_elevated = match engine::resolve(store, &rule) {
                Ok(r) => crate::safety::requires_elevation(&r.source),
                Err(_) => false,
            };
            if !scope.accepts(source_elevated) {
                continue;
            }
        }
        let root = store.get_root(rule.target_root_id)?;
        pending.push((rule, root));
    }
    pending.sort_by_key(|(r, _)| r.target_subpath.matches(['\\', '/']).count());
    report.rules_total = pending.len();

    // ---- 3. 逐条重建 ----
    let mut noop = |_: Progress| {};
    let _ = &mut noop;

    for (rule, root) in &pending {
        match rebuild_one(store, rule, root) {
            Ok((created, RebuildAction::AlreadyPresent)) => {
                if created {
                    report.dirs_created += 1;
                    report.links_recovered += 1;
                } else {
                    report.already_ok += 1;
                }
            }
            Ok((created, RebuildAction::Relinked)) => {
                if created {
                    report.dirs_created += 1;
                }
                report.links_repaired += 1;
            }
            Ok((created, RebuildAction::NeedsAttention(w))) => {
                if created {
                    report.dirs_created += 1;
                }
                report.warnings.push(tf("boot.warning", &[
                    ("name", &rule.name),
                    ("msg", &w),
                ]));
            }
            Err(e) => report.failed.push((rule.id, e.to_string())),
        }
    }

    report.elapsed_ms = started.elapsed().as_millis();
    store.log_event(
        None,
        if report.has_problem() { "warn" } else { "info" },
        "boot_rebuild",
        &report.one_line(),
    )?;

    // 落盘一份结果：登录任务是无界面跑的，失败必须能在下次打开界面时看到，
    // 而不是只躺在事件日志里等用户去翻。
    if let Ok(json) = serde_json::to_string(&report) {
        let (report_key, at_key) = scope.report_keys();
        let _ = store.set_setting(report_key, &json);
        let _ = store.set_setting(at_key, &store.now().unwrap_or_default());
    }
    Ok(report)
}

/// 处理单条规则。返回 `(是否新建了目标目录, 动作)`。
fn rebuild_one(store: &Store, rule: &Rule, root: &Root) -> TdResult<(bool, RebuildAction)> {
    let r = engine::resolve(store, rule)?;
    if !r.unknown_vars.is_empty() {
        return Ok((
            false,
            RebuildAction::NeedsAttention(tf("boot.unresolved_vars", &[
                ("vars", &r.unknown_vars.join("、")),
            ])),
        ));
    }
    let (root_path, _) = root.resolved_path();
    if !root_path.exists() {
        return Ok((
            false,
            RebuildAction::NeedsAttention(tf("boot.root_unavailable", &[
                ("path", &root_path.display().to_string()),
            ])),
        ));
    }

    // 建目录（这一步是开机守护的核心职责）
    let created = ensure_dir(&r.target)?;

    // 需要提权的规则：把首次重定向时导出的 DACL 贴回去
    if let Some(sddl) = rule.acl_sddl.as_deref() {
        if let Err(e) = acl::apply_sddl(&r.target, sddl) {
            store.log_event(
                Some(rule.id),
                "warn",
                "acl_failed",
                &tf("boot.acl_failed", &[("name", &rule.name), ("err", &e.to_string())]),
            )?;
        }
    }

    let state = detect_link(&r.source, Some(&r.target));
    match state {
        // 目录刚建好，链接必然有效（目标为空）
        LinkState::LinkOk { .. } => Ok((created, RebuildAction::AlreadyPresent)),
        LinkState::LinkBroken { .. } => {
            // 极少见：目录建好了链接还断，说明链接本身有问题
            let again = detect_link(&r.source, Some(&r.target));
            if again.is_managed_link() {
                Ok((created, RebuildAction::AlreadyPresent))
            } else {
                Ok((created, RebuildAction::NeedsAttention(again.label())))
            }
        }
        LinkState::SourceMissing => {
            // 源完全不存在。父目录还在的话直接把链接建出来 —— 软件装好即可用。
            let parent_ok = r
                .source
                .parent()
                .map(|p| p.as_os_str().is_empty() || p.exists())
                .unwrap_or(false);
            if parent_ok {
                let lt = rule_link_type(rule)?;
                create_dir_link(&r.source, &r.target, lt)?;
                Ok((created, RebuildAction::Relinked))
            } else {
                Ok((
                    created,
                    RebuildAction::NeedsAttention(t("boot.source_and_parent_missing").into()),
                ))
            }
        }
        LinkState::PlainDir => Ok((
            created,
            RebuildAction::NeedsAttention(t("boot.source_became_plain").into()),
        )),
        LinkState::ForeignReparse { tag } => Ok((
            created,
            RebuildAction::NeedsAttention(tf("boot.foreign_reparse", &[
                ("tag", &format!("{tag:#010x}")),
            ])),
        )),
        other => Ok((created, RebuildAction::NeedsAttention(other.label()))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::{apply_rule, ApplyOptions};
    use crate::fsops::mover::Progress;
    use crate::store::{NewRule, Store};
    use std::fs;
    use std::path::PathBuf;

    struct Env {
        base: PathBuf,
        store: Store,
        rule_id: i64,
        source: PathBuf,
        target: PathBuf,
    }

    fn setup(tag: &str) -> Env {
        let base = std::env::temp_dir().join(format!("td_boot_{tag}_{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        let source = base.join("cache");
        let ram = base.join("ram");
        fs::create_dir_all(&source).unwrap();
        fs::create_dir_all(&ram).unwrap();
        fs::write(source.join("data.bin"), vec![3u8; 512]).unwrap();

        let store = Store::open_in_memory().unwrap();
        let root_id = store
            .insert_root("RAM", &ram.to_string_lossy(), true, 5000)
            .unwrap();
        let rule_id = store
            .insert_rule(&NewRule {
                name: "开机测试".into(),
                category: "dev".into(),
                source_template: source.to_string_lossy().into_owned(),
                target_root_id: root_id,
                target_subpath: "dev\\boot-cache".into(),
                link_type: "junction".into(),
                want_enabled: true,
                rebuild_on_boot: true,
                notes: None,
                excludes: Vec::new(),
                auto_clean_policy: String::new(),
                auto_clean_limit_bytes: 0,
            })
            .unwrap();

        Env {
            base,
            store,
            rule_id,
            source,
            target: ram.join("dev\\boot-cache"),
        }
    }

    #[test]
    fn rebuild_recreates_target_dir_after_ram_loss() {
        let env = setup("ramloss");
        let rule = env.store.get_rule(env.rule_id).unwrap();
        let mut noop = |_: Progress| {};
        apply_rule(&env.store, &rule, &ApplyOptions::default(), &mut noop).unwrap();
        assert!(env.target.exists());

        // 模拟 RAM 盘重启：目标目录整个消失
        fs::remove_dir_all(&env.target).unwrap();

        let report = run_boot_rebuild(&env.store).unwrap();
        assert!(env.target.exists(), "开机重建必须把目标目录建回来");
        assert_eq!(report.dirs_created, 1);
        assert_eq!(report.links_recovered, 1);
        assert_eq!(report.already_ok, 0);
        assert!(!report.has_problem(), "{}", report.one_line());
        // 链接恢复可用
        assert!(matches!(
            detect_link(&env.source, Some(&env.target)),
            LinkState::LinkOk { .. }
        ));

        let _ = fs::remove_dir_all(&env.base);
    }

    #[test]
    fn rebuild_recreates_subfolder_root_after_ram_loss() {
        // 这个用例覆盖的是一条真会踩到的路径：目标根是 RAM 盘**下的子目录**
        // （文档里就推荐 `R:\Redirect` 这种写法），RAM 盘重启后连根目录一起没了。
        // 老实现只"等目录自己出现"，于是永远等不到、整条重建被跳过。
        let base = std::env::temp_dir().join(format!("td_boot_subroot_{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        let source = base.join("cache");
        fs::create_dir_all(&source).unwrap();
        fs::write(source.join("a.txt"), b"x").unwrap();

        // "RAM 盘"用 base\ram2 代表，目标根是它下面的子目录 Redirect
        fs::create_dir_all(base.join("ram2")).unwrap();
        let root_path = base.join("ram2").join("Redirect");

        let store = Store::open_in_memory().unwrap();
        let root_id = store
            .insert_root("RAM", &root_path.to_string_lossy(), true, 3000)
            .unwrap();
        let rule_id = store
            .insert_rule(&NewRule {
                name: "子目录根".into(),
                category: "dev".into(),
                source_template: source.to_string_lossy().into_owned(),
                target_root_id: root_id,
                target_subpath: "dev\\cache".into(),
                link_type: "junction".into(),
                want_enabled: true,
                rebuild_on_boot: true,
                notes: None,
                excludes: Vec::new(),
                auto_clean_policy: String::new(),
                auto_clean_limit_bytes: 0,
            })
            .unwrap();

        // 1) 根目录还不存在 → apply 应该顺手建出来，而不是报"目标根不可用"
        let rule = store.get_rule(rule_id).unwrap();
        let mut noop = |_: Progress| {};
        let out = apply_rule(&store, &rule, &ApplyOptions::default(), &mut noop).unwrap();
        assert_eq!(out.action, "moved_and_linked");
        assert!(root_path.is_dir(), "apply 应把不存在的目标根建出来");

        // 2) 模拟 RAM 盘重启：整个 ram2 消失，然后盘回来、但根目录不在
        fs::remove_dir_all(base.join("ram2")).unwrap();
        fs::create_dir_all(base.join("ram2")).unwrap();

        let report = run_boot_rebuild(&store).unwrap();
        assert!(root_path.is_dir(), "开机重建必须把根目录建出来，不能干等");
        assert!(root_path.join("dev").join("cache").is_dir());
        assert_eq!(report.dirs_created, 1);
        assert!(!report.has_problem(), "{}", report.one_line());

        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn rebuild_reports_unavailable_root_without_touching_rules() {
        let env = setup("badroot");
        env.store
            .update_root(1, "RAM", "Z:\\NotExistAtAll", true, 1000)
            .unwrap();
        let report = run_boot_rebuild(&env.store).unwrap();
        assert!(report.roots[0].error.is_some());
        assert!(report.has_problem());
        // 源目录原封不动
        assert!(env.source.join("data.bin").exists());
        let _ = fs::remove_dir_all(&env.base);
    }

    #[test]
    fn rebuild_warns_on_plain_dir_instead_of_touching() {
        let env = setup("plain");
        // 源是真实目录（未托管）
        let report = run_boot_rebuild(&env.store).unwrap();
        assert_eq!(report.warnings.len(), 1);
        assert!(report.warnings[0].contains("真实目录"));
        assert!(env.source.join("data.bin").exists());
        assert!(env.target.exists(), "目标目录仍然应该被建出来");
        let _ = fs::remove_dir_all(&env.base);
    }

    #[test]
    fn rebuild_skips_when_nothing_enabled() {
        let env = setup("disabled");
        env.store.set_rule_enabled(env.rule_id, false).unwrap();
        let report = run_boot_rebuild(&env.store).unwrap();
        assert_eq!(report.rules_total, 0);
        assert!(!env.target.exists());
        let _ = fs::remove_dir_all(&env.base);
    }

    #[test]
    fn scope_splits_protected_and_normal_rules() {
        let env = setup("scope");
        let win = std::env::var("SystemRoot").unwrap_or_else(|_| "C:\\Windows".into());
        env.store
            .insert_rule(&NewRule {
                name: "受保护源".into(),
                category: "system".into(),
                source_template: format!("{win}\\Temp\\td-scope-{}", std::process::id()),
                target_root_id: 1,
                target_subpath: "system\\protected".into(),
                link_type: "junction".into(),
                want_enabled: true,
                rebuild_on_boot: true,
                notes: None,
                excludes: Vec::new(),
                auto_clean_policy: String::new(),
                auto_clean_limit_bytes: 0,
            })
            .unwrap();

        let non_elevated = run_boot_rebuild_scoped(&env.store, BootScope::NonElevatedOnly).unwrap();
        assert_eq!(non_elevated.rules_total, 1, "普通任务只管普通源");

        let elevated = run_boot_rebuild_scoped(&env.store, BootScope::ElevatedOnly).unwrap();
        assert_eq!(elevated.rules_total, 1, "高权限任务只管受保护源");

        let all = run_boot_rebuild_scoped(&env.store, BootScope::All).unwrap();
        assert_eq!(all.rules_total, 2);

        // 两份报告写不同的键，界面才能同时看到
        assert!(env.store.get_setting("last_boot_report").unwrap().is_some());
        assert!(env.store
            .get_setting("last_boot_report_elevated")
            .unwrap()
            .is_some());
        let _ = fs::remove_dir_all(&env.base);
    }
}
