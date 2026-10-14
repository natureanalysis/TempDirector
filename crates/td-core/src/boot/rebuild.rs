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

use serde::Serialize;

use crate::boot::acl;
use crate::boot::volume::wait_for_volume;
use crate::engine::{self, rule_link_type};
use crate::error::TdResult;
use crate::fsops::mover::Progress;
use crate::link::create::{create_dir_link, ensure_dir};
use crate::link::detect::{detect_link, LinkState};
use crate::store::{Root, Rule, Store};

#[derive(Debug, Clone, Serialize)]
pub struct RootOutcome {
    pub root_id: i64,
    pub label: String,
    pub path: String,
    pub ready: bool,
    pub waited_ms: u128,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize)]
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
        format!(
            "目标卷 {}/{} 就绪；规则 {} 条：新建目录 {}，链接自动恢复 {}，链接重建 {}，本就正常 {}，告警 {}，失败 {}（耗时 {}）",
            self.roots.iter().filter(|r| r.ready).count(),
            self.roots.len(),
            self.rules_total,
            self.dirs_created,
            self.links_recovered,
            self.links_repaired,
            self.already_ok,
            self.warnings.len(),
            self.failed.len(),
            crate::util::human_dur_ms(self.elapsed_ms)
        )
    }
}

enum RebuildAction {
    AlreadyPresent,
    Relinked,
    NeedsAttention(String),
}

/// 执行一次开机重建。
pub fn run_boot_rebuild(store: &Store) -> TdResult<BootReport> {
    let started = Instant::now();
    let mut report = BootReport::default();

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
            let msg = format!("路径里的环境变量无法解析：{}", unknown.join("、"));
            outcome.error = Some(msg.clone());
            let _ = store.log_event(None, "error", "root_bad_path", &msg);
            report.roots.push(outcome);
            continue;
        }

        let timeout = Duration::from_millis(root.wait_timeout_ms.max(1000) as u64);
        match wait_for_volume(&path, timeout, Duration::from_millis(500)) {
            Ok(waited) => {
                outcome.ready = true;
                outcome.waited_ms = waited.as_millis();
                ready_ids.push(root.id);
                let _ = store.log_event(
                    None,
                    "info",
                    "root_ready",
                    &format!("{} 就绪（等待 {:.1}s）", path.display(), waited.as_secs_f64()),
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
                report.warnings.push(format!("{}：{w}", rule.name));
            }
            Err(e) => report.failed.push((rule.id, e.to_string())),
        }
    }

    report.elapsed_ms = started.elapsed().as_millis();
    let _ = store.log_event(
        None,
        if report.has_problem() { "warn" } else { "info" },
        "boot_rebuild",
        &report.one_line(),
    )?;
    Ok(report)
}

/// 处理单条规则。返回 `(是否新建了目标目录, 动作)`。
fn rebuild_one(store: &Store, rule: &Rule, root: &Root) -> TdResult<(bool, RebuildAction)> {
    let r = engine::resolve(store, rule)?;
    if !r.unknown_vars.is_empty() {
        return Ok((
            false,
            RebuildAction::NeedsAttention(format!(
                "环境变量无法解析：{}（这条规则可能属于其他用户）",
                r.unknown_vars.join("、")
            )),
        ));
    }
    let (root_path, _) = root.resolved_path();
    if !root_path.exists() {
        return Ok((
            false,
            RebuildAction::NeedsAttention(format!("目标根不可用：{}", root_path.display())),
        ));
    }

    // 建目录（这一步是开机守护的核心职责）
    let created = ensure_dir(&r.target)?;

    // 需要提权的规则：把首次重定向时导出的 DACL 贴回去
    if let Some(sddl) = rule.acl_sddl.as_deref() {
        if let Err(e) = acl::apply_sddl(&r.target, sddl) {
            store.log_event(Some(rule.id), "warn", "acl_failed", &format!("{}：{e}", rule.name))?;
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
                Ok((created, RebuildAction::NeedsAttention(again.label_zh())))
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
                    RebuildAction::NeedsAttention("源目录及其父目录都不存在（软件可能未安装）".into()),
                ))
            }
        }
        LinkState::PlainDir => Ok((
            created,
            RebuildAction::NeedsAttention(
                "源路径已变回真实目录（可能被软件更新覆盖），未自动处理，请在界面上重新应用".into(),
            ),
        )),
        LinkState::ForeignReparse { tag } => Ok((
            created,
            RebuildAction::NeedsAttention(format!("源路径是外来重解析点（tag {tag:#010x}）")),
        )),
        other => Ok((created, RebuildAction::NeedsAttention(other.label_zh()))),
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
}
