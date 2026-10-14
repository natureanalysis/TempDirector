//! 编排层：把「规则 + 目标根」翻译成一次可审计、可回滚的操作。
//!
//! ## 应用一条规则的决策树（`apply_rule`）
//!
//! | 当前状态 | 动作 |
//! |---|---|
//! | 链接正常 + 目标有内容 | 幂等返回，什么都不做 |
//! | 链接正常 + 目标为空 | 幂等返回（RAM 盘刚重启的正常形态） |
//! | 链接正常但指向与登记不符 | 拒绝，要求先恢复 |
//! | 链接断裂（目标丢失） | 重建目标目录，链接即恢复 |
//! | 源不存在 | 建目标目录 + 直接建链（软件首次运行就会落到 RAM 盘） |
//! | 源是普通目录 + 目标空 | 搬运 → 校验 → 建链 |
//! | 源是普通目录 + 目标有内容 | 合并（新者胜）→ 删源 → 建链 |
//! | 外来重解析点 | 拒绝，绝不触碰 |
//!
//! ## 失败一定回滚
//!
//! 建链失败会把已搬过去的内容**搬回来**；搬不回来则留下一条非终态的操作流水，
//! 下次启动时作为「待修复」提示给用户，而不是静默留下一个半成品。

use std::path::{Path, PathBuf};
use std::time::Instant;

use serde::Serialize;

use crate::error::{TdError, TdResult};
use crate::fsops::mover::{self, MoveReport, Progress};
use crate::fsops::occupancy::{self, LockScanOptions};
use crate::fsops::verify::{self, TreeStats, VerifyMode};
use crate::link::create::{create_dir_link, ensure_dir, remove_dir_link};
use crate::link::detect::{detect_link, LinkState, LinkType, TargetState};
use crate::store::{Root, Rule, Store};
use crate::util;

/// 一条规则的实时体检结果。
#[derive(Debug, Clone, Serialize)]
pub struct RuleStatus {
    pub rule_id: i64,
    pub name: String,
    pub category: String,
    pub source: String,
    pub target: String,
    pub root: String,
    pub link_type: String,
    pub want_enabled: bool,
    pub rebuild_on_boot: bool,
    /// UI 徽标（✅ / ⚠️ / ❌ / 🚫 / ❔），直接来自 `LinkState::badge()`。
    pub badge: &'static str,
    pub label: String,
    pub state: LinkState,
    pub needs_attention: bool,
    pub warnings: Vec<String>,
    pub unknown_vars: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct Resolved {
    pub root: Root,
    pub source: PathBuf,
    pub target: PathBuf,
    pub warnings: Vec<String>,
    pub unknown_vars: Vec<String>,
}

/// 解析一条规则的源与目标（展开环境变量）。
pub fn resolve(store: &Store, rule: &Rule) -> TdResult<Resolved> {
    let root = store.get_root(rule.target_root_id)?;
    let (source, mut unknown) = rule.resolved_source();
    let (target, unk2) = rule.resolved_target(&root);
    unknown.extend(unk2);

    let mut warnings = Vec::new();
    if !unknown.is_empty() {
        warnings.push(format!(
            "存在未解析的环境变量：{}（该规则可能属于其他用户）",
            unknown.join("、")
        ));
    }
    let pf = crate::safety::preflight_source(&source);
    warnings.extend(pf.warnings.clone());

    Ok(Resolved {
        root,
        source,
        target,
        warnings,
        unknown_vars: unknown,
    })
}

pub fn rule_link_type(rule: &Rule) -> TdResult<LinkType> {
    LinkType::parse(&rule.link_type)
        .ok_or_else(|| TdError::InvalidPath(format!("未知的链接类型：{}", rule.link_type)))
}

/// 执行前的「环境就绪」校验。
fn validate_ready(r: &Resolved) -> TdResult<()> {
    if !r.unknown_vars.is_empty() {
        return Err(TdError::InvalidPath(format!(
            "源路径里的环境变量无法解析：{}。请以目标用户的身份运行，或改用绝对路径。",
            r.unknown_vars.join("、")
        )));
    }
    let (root_path, _) = r.root.resolved_path();
    if !root_path.exists() {
        return Err(TdError::Other(format!(
            "目标根不可用：{}（RAM 盘未挂载？请先创建并格式化该盘）",
            root_path.display()
        )));
    }
    crate::safety::validate_rule(&r.source, &r.target)?;
    Ok(())
}

fn dir_has_content(dir: &Path) -> bool {
    std::fs::read_dir(&util::to_verbatim(dir))
        .map(|mut it| it.next().is_some())
        .unwrap_or(false)
}

// ---------------------------------------------------------------------------
// 体检
// ---------------------------------------------------------------------------

pub fn rule_status(store: &Store, rule: &Rule) -> TdResult<RuleStatus> {
    let r = resolve(store, rule)?;
    let state = detect_link(&r.source, Some(&r.target));
    Ok(RuleStatus {
        rule_id: rule.id,
        name: rule.name.clone(),
        category: rule.category.clone(),
        source: r.source.display().to_string(),
        target: r.target.display().to_string(),
        root: r.root.label.clone(),
        link_type: rule.link_type.clone(),
        want_enabled: rule.want_enabled,
        rebuild_on_boot: rule.rebuild_on_boot,
        badge: state.badge(),
        label: state.label_zh(),
        needs_attention: state.needs_attention(),
        warnings: r.warnings.clone(),
        unknown_vars: r.unknown_vars.clone(),
        state,
    })
}

/// 批量体检。`persist` 为真时把结果写回规则库的状态缓存。
pub fn status_all(
    store: &Store,
    category: Option<&str>,
    enabled_only: bool,
    keyword: Option<&str>,
    persist: bool,
) -> TdResult<Vec<RuleStatus>> {
    let rules = store.query_rules(category, enabled_only, keyword)?;
    let mut out = Vec::with_capacity(rules.len());
    for rule in &rules {
        let st = rule_status(store, rule)?;
        if persist {
            let err = match &st.state {
                LinkState::IoError { message } => Some(message.as_str()),
                _ => None,
            };
            store.set_rule_state(rule.id, state_key(&st.state), err)?;
        }
        out.push(st);
    }
    Ok(out)
}

/// 状态机键（写进 DB 的 `last_state`）。
pub fn state_key(state: &LinkState) -> &'static str {
    match state {
        LinkState::SourceMissing => "source_missing",
        LinkState::PlainDir => "plain_dir",
        LinkState::PlainFile => "plain_file",
        LinkState::LinkOk {
            target_state: TargetState::Empty,
            ..
        } => "link_ok_empty",
        LinkState::LinkOk { .. } => "link_ok",
        LinkState::LinkBroken { .. } => "link_broken",
        LinkState::ForeignReparse { .. } => "foreign_reparse",
        LinkState::AccessDenied => "access_denied",
        LinkState::IoError { .. } => "io_error",
    }
}

// ---------------------------------------------------------------------------
// 应用
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct ApplyOptions {
    pub dry_run: bool,
    pub verify: VerifyMode,
    /// 跳过占用检测（危险：源可能正在被写入，副本会撕裂）。
    pub skip_lock_check: bool,
    /// 允许把源内容合并进非空的目标目录。
    pub allow_merge: bool,
}

impl Default for ApplyOptions {
    fn default() -> Self {
        Self {
            dry_run: false,
            verify: VerifyMode::CountAndSize,
            skip_lock_check: false,
            allow_merge: false,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Outcome {
    pub rule_id: i64,
    pub name: String,
    /// already_ok / fixed_broken_target / created_and_linked / moved_and_linked /
    /// merged_and_linked / restored / already_plain / relinked / dry_run
    pub action: &'static str,
    pub message: String,
    pub stats: Option<TreeStats>,
    pub links_recreated: u64,
    pub elapsed_ms: u128,
    pub dry_run: bool,
}

fn outcome(
    rule: &Rule,
    action: &'static str,
    message: String,
    stats: Option<TreeStats>,
    started: Instant,
    dry_run: bool,
) -> Outcome {
    Outcome {
        rule_id: rule.id,
        name: rule.name.clone(),
        action,
        message,
        stats,
        links_recreated: 0,
        elapsed_ms: started.elapsed().as_millis(),
        dry_run,
    }
}

/// 应用一条规则：把源目录变成指向 RAM 盘的链接。
pub fn apply_rule(
    store: &Store,
    rule: &Rule,
    opts: &ApplyOptions,
    sink: &mut dyn FnMut(Progress),
) -> TdResult<Outcome> {
    let started = Instant::now();
    let r = resolve(store, rule)?;
    validate_ready(&r)?;
    let lt = rule_link_type(rule)?;

    let state = detect_link(&r.source, Some(&r.target));

    // ---- 幂等 / 快速分支 ----
    match &state {
        LinkState::LinkOk {
            target_state: TargetState::HasContent,
            target_mismatch: false,
            ..
        } => {
            store.set_rule_state(rule.id, "link_ok", None)?;
            return Ok(outcome(
                rule,
                "already_ok",
                "链接已就绪且缓存有内容，无需操作".into(),
                None,
                started,
                false,
            ));
        }
        LinkState::LinkOk {
            target_state: TargetState::Empty,
            target_mismatch: false,
            ..
        } => {
            store.set_rule_state(rule.id, "link_ok_empty", None)?;
            return Ok(outcome(
                rule,
                "already_ok",
                "链接已就绪（目标为空属正常：RAM 盘重启后缓存会自然重建）".into(),
                None,
                started,
                false,
            ));
        }
        LinkState::LinkOk {
            target_mismatch: true,
            target,
            ..
        } => {
            return Err(TdError::Other(format!(
                "链接实际指向 {}，与登记的目标 {} 不一致。请先执行「恢复」再重新应用。",
                target.display(),
                r.target.display()
            )));
        }
        LinkState::ForeignReparse { tag } => {
            return Err(TdError::SafetyBlocked(format!(
                "源路径是外来重解析点（tag {tag:#010x}，可能是云盘占位符），本工具不会触碰它：{}",
                r.source.display()
            )));
        }
        LinkState::PlainFile => {
            return Err(TdError::InvalidPath(format!(
                "源路径是文件而不是目录，请修正规则：{}",
                r.source.display()
            )));
        }
        LinkState::AccessDenied => {
            return Err(TdError::ElevationRequired(format!(
                "读取源目录被拒绝，请以管理员身份重试：{}",
                r.source.display()
            )));
        }
        LinkState::IoError { message } => {
            return Err(TdError::Other(message.clone()));
        }
        LinkState::LinkBroken { .. } => {
            // 链接本身完好，只是目标目录没了 —— 建回来即可。
            if opts.dry_run {
                return Ok(outcome(
                    rule,
                    "fixed_broken_target",
                    "将重建丢失的目标目录".into(),
                    None,
                    started,
                    true,
                ));
            }
            ensure_dir(&r.target)?;
            store.set_rule_state(rule.id, "link_ok_empty", None)?;
            store.log_event(
                Some(rule.id),
                "warn",
                "target_rebuilt",
                &format!("目标目录 {} 已重建，链接恢复可用", r.target.display()),
            )?;
            return Ok(outcome(
                rule,
                "fixed_broken_target",
                format!(
                    "已重建目标目录 {}，链接恢复可用（原缓存内容随 RAM 盘重启丢失，属预期）",
                    r.target.display()
                ),
                None,
                started,
                false,
            ));
        }
        LinkState::SourceMissing => {
            if opts.dry_run {
                return Ok(outcome(
                    rule,
                    "created_and_linked",
                    "将创建目标目录并直接建立链接".into(),
                    None,
                    started,
                    true,
                ));
            }
            let created = ensure_dir(&r.target)?;
            create_dir_link(&r.source, &r.target, lt)?;
            store.set_rule_state(rule.id, "link_ok_empty", None)?;
            store.log_event(
                Some(rule.id),
                "info",
                "linked_without_source",
                &format!(
                    "源目录原本不存在，已直接建立链接（目标目录{}）",
                    if created { "已创建" } else { "已存在" }
                ),
            )?;
            return Ok(outcome(
                rule,
                "created_and_linked",
                format!(
                    "源目录不存在，已直接建立链接；目标目录就绪：{}",
                    r.target.display()
                ),
                None,
                started,
                false,
            ));
        }
        // 兜底：只剩 target_state=Missing 且 mismatch=false 这一种组合。
        // 按 `detect_link` 的语义，目标不可达时应该产出 LinkBroken，
        // 所以这个分支逻辑上不可达；保守起见报错而不是猜一个动作。
        LinkState::LinkOk {
            target_state: TargetState::Missing,
            ..
        } => {
            return Err(TdError::Other(format!(
                "链接状态自相矛盾（链接可用但目标状态为缺失）：{}",
                r.source.display()
            )));
        }
        LinkState::PlainDir => { /* 走下面的搬运流程 */ }
    }

    // ---- 需要搬运 ----
    let src_stats = verify::measure(&r.source)?;
    let target_non_empty = dir_has_content(&r.target);

    if target_non_empty && !opts.allow_merge {
        return Err(TdError::SafetyBlocked(format!(
            "目标目录已有内容：{}。加 --merge 可以把源目录合并进去（同路径文件取修改时间较新者）后建链。",
            r.target.display()
        )));
    }

    // 占用检测：证明源目录此刻没有被写入，副本才不会撕裂。
    if !opts.skip_lock_check {
        let report = occupancy::scan_locks(&r.source, &LockScanOptions::default())?;
        if !report.is_clear() {
            store.log_event(
                Some(rule.id),
                "warn",
                "blocked",
                &format!("{}：{}", rule.name, report.summary()),
            )?;
            return Err(TdError::Busy(format!(
                "源目录正被占用，已中止（未做任何改动）：{}",
                report.summary()
            )));
        }
    }

    if opts.dry_run {
        let action = if target_non_empty {
            "merged_and_linked"
        } else {
            "moved_and_linked"
        };
        let what = if target_non_empty {
            "将合并进已有目标目录后建链"
        } else {
            "将搬运到目标目录后建链"
        };
        return Ok(outcome(rule, action, what.into(), Some(src_stats), started, true));
    }

    // ---- 正式执行（带流水与回滚）----
    let op = store.op_begin(
        Some(rule.id),
        "apply",
        Some(&r.source.display().to_string()),
        Some(&r.target.display().to_string()),
    )?;

    let mut action: &'static str = "moved_and_linked";
    let mut note = String::new();
    let mut move_report: Option<MoveReport> = None;

    let work = (|| -> TdResult<()> {
        if target_non_empty {
            store.op_phase(op, "copying", None)?;
            let mr = mover::merge_into(&r.source, &r.target, sink)?;
            action = "merged_and_linked";
            note = format!(
                "已合并 {} 个文件进目标目录（覆盖 {} 个较旧文件，跳过 {} 个不较新的）",
                mr.copied + mr.overwritten,
                mr.overwritten,
                mr.skipped
            );
            store.op_phase(op, "copied", None)?;
            store.op_phase(op, "removing", None)?;
            mover::delete_tree(&r.source, sink)?;
        } else {
            store.op_phase(op, "copying", None)?;
            let mr = mover::move_tree(&r.source, &r.target, opts.verify, sink)?;
            note = format!("{}：{}", mr.method, mr.stats.summary());
            move_report = Some(mr);
            store.op_phase(op, "copied", None)?;
        }
        store.op_phase(op, "linking", None)?;
        sink(Progress::Linking {
            link: r.source.display().to_string(),
            target: r.target.display().to_string(),
        });
        create_dir_link(&r.source, &r.target, lt)?;
        Ok(())
    })();

    if let Err(e) = work {
        match rollback_apply(&r, target_non_empty) {
            Ok(rb) => {
                let _ = store.op_finish(
                    op,
                    "rolled_back",
                    None,
                    None,
                    Some(&format!("{e}；回滚：{rb}")),
                );
                let _ = store.set_rule_state(rule.id, "rollback", Some(&e.to_string()));
                let _ = store.log_event(
                    Some(rule.id),
                    "error",
                    "apply_failed",
                    &format!("{}：{e}（已回滚：{rb}）", rule.name),
                );
            }
            Err(rb) => {
                // 回滚也失败 → 留一条非终态流水，下次启动作为「待人工处理」
                let _ = store.op_phase(
                    op,
                    "needs_manual",
                    Some(&format!("{e}；回滚失败：{rb}")),
                );
                let _ = store.log_event(
                    Some(rule.id),
                    "error",
                    "needs_manual",
                    &format!(
                        "{}：{e}；且回滚失败：{rb}。请人工检查 {} 与 {}",
                        rule.name,
                        r.source.display(),
                        r.target.display()
                    ),
                );
            }
        }
        return Err(e);
    }

    let stats = move_report.as_ref().map(|m| m.stats).or(Some(src_stats));
    let links_recreated = move_report.as_ref().map(|m| m.links_recreated).unwrap_or(0);
    let files = stats.map(|s| s.files as i64).unwrap_or(0);
    let bytes = stats.map(|s| s.bytes as i64).unwrap_or(0);

    store.op_finish(op, "done", Some(files), Some(bytes), None)?;
    store.set_rule_applied(rule.id, files, bytes)?;
    store.log_event(
        Some(rule.id),
        "info",
        "applied",
        &format!("{}：{note}", rule.name),
    )?;

    let mut o = outcome(
        rule,
        action,
        format!(
            "{note}；已建链 {} → {}",
            r.source.display(),
            r.target.display()
        ),
        stats,
        started,
        false,
    );
    o.links_recreated = links_recreated;
    Ok(o)
}

/// 应用失败时的回滚。
fn rollback_apply(r: &Resolved, merged: bool) -> Result<String, String> {
    // 先尽力清掉可能半成的链接（不存在会报错，忽略）
    let _ = remove_dir_link(&r.source);

    if merged {
        // 合并路径：数据都在目标目录里。只需保证源路径存在，缓存可重造。
        if !r.source.exists() {
            ensure_dir(&r.source).map_err(|e| e.to_string())?;
            return Ok("已把源目录重建为空目录（合并进 RAM 盘的内容保留）".into());
        }
        return Ok("源目录仍存在，无需处理".into());
    }

    if !dir_has_content(&r.target) {
        return Ok("目标目录为空，无需搬回".into());
    }
    if r.source.exists() {
        return Ok("源目录已存在，跳过搬回".into());
    }

    let mut noop = |_: Progress| {};
    let mr = mover::move_tree(&r.target, &r.source, VerifyMode::CountAndSize, &mut noop)
        .map_err(|e| e.to_string())?;
    Ok(format!("已把 {} 搬回源目录", mr.stats.summary()))
}

// ---------------------------------------------------------------------------
// 恢复
// ---------------------------------------------------------------------------

/// 恢复一条规则：拆掉链接，把 RAM 盘里的内容搬回原位。
pub fn restore_rule(
    store: &Store,
    rule: &Rule,
    opts: &ApplyOptions,
    sink: &mut dyn FnMut(Progress),
) -> TdResult<Outcome> {
    let started = Instant::now();
    let r = resolve(store, rule)?;
    let lt = rule_link_type(rule)?;
    crate::safety::preflight_pair(&r.source, &r.target)?;

    let state = detect_link(&r.source, Some(&r.target));

    match &state {
        LinkState::PlainDir => {
            store.set_rule_state(rule.id, "plain_dir", None)?;
            return Ok(outcome(
                rule,
                "already_plain",
                "本来就是真实目录，无需恢复".into(),
                None,
                started,
                false,
            ));
        }
        LinkState::SourceMissing => {
            store.set_rule_state(rule.id, "source_missing", None)?;
            return Ok(outcome(
                rule,
                "already_plain",
                "源路径不存在，无需恢复".into(),
                None,
                started,
                false,
            ));
        }
        LinkState::ForeignReparse { tag } => {
            return Err(TdError::SafetyBlocked(format!(
                "源路径是外来重解析点（tag {tag:#010x}），本工具不会触碰它"
            )));
        }
        LinkState::PlainFile => {
            return Err(TdError::InvalidPath(format!(
                "源路径是文件，无法恢复：{}",
                r.source.display()
            )));
        }
        LinkState::AccessDenied => {
            return Err(TdError::ElevationRequired(format!(
                "读取源目录被拒绝，请以管理员身份重试：{}",
                r.source.display()
            )));
        }
        LinkState::IoError { message } => return Err(TdError::Other(message.clone())),
        LinkState::LinkOk { .. } | LinkState::LinkBroken { .. } => { /* 走恢复流程 */ }
    }

    let has_content = dir_has_content(&r.target);
    let plan = if has_content {
        "将拆掉链接并把 RAM 盘中的内容搬回原位"
    } else {
        "将拆掉链接并重建空的原目录"
    };
    if opts.dry_run {
        return Ok(outcome(rule, "restored", plan.into(), None, started, true));
    }

    let op = store.op_begin(
        Some(rule.id),
        "restore",
        Some(&r.source.display().to_string()),
        Some(&r.target.display().to_string()),
    )?;

    // 1) 拆链接（只删链接条目，不碰目标内容）
    let target = match remove_dir_link(&r.source) {
        Ok(t) => t,
        Err(e) => {
            let _ = store.op_finish(op, "failed", None, None, Some(&e.to_string()));
            return Err(e);
        }
    };
    store.op_phase(op, "removing", None)?;

    // 2) 内容搬回（或建空目录）
    let stats = if has_content {
        match mover::move_tree(&target, &r.source, opts.verify, sink) {
            Ok(mr) => {
                store.op_phase(op, "linking", None)?;
                mr.stats
            }
            Err(e) => {
                // 回滚：把链接建回去，回到「已重定向」的状态
                match create_dir_link(&r.source, &target, lt) {
                    Ok(()) => {
                        let _ = store.op_finish(
                            op,
                            "rolled_back",
                            None,
                            None,
                            Some(&format!("{e}；已把链接建回，状态未变")),
                        );
                        let _ = store.set_rule_state(rule.id, "link_ok", Some(&e.to_string()));
                        let _ = store.log_event(
                            Some(rule.id),
                            "error",
                            "restore_failed",
                            &format!("{}：{e}（已把链接建回）", rule.name),
                        );
                    }
                    Err(rb) => {
                        let _ = store.op_phase(
                            op,
                            "needs_manual",
                            Some(&format!("{e}；且链接未能建回：{rb}")),
                        );
                        let _ = store.log_event(
                            Some(rule.id),
                            "error",
                            "needs_manual",
                            &format!(
                                "{}：{e}；且链接未能建回：{rb}。RAM 盘内容仍在 {}，原路径 {}",
                                rule.name,
                                target.display(),
                                r.source.display()
                            ),
                        );
                    }
                }
                return Err(e);
            }
        }
    } else {
        ensure_dir(&r.source)?;
        TreeStats::default()
    };

    store.op_finish(
        op,
        "done",
        Some(stats.files as i64),
        Some(stats.bytes as i64),
        None,
    )?;
    store.set_rule_state(rule.id, "plain_dir", None)?;
    store.log_event(
        Some(rule.id),
        "info",
        "restored",
        &format!("{}：{plan}（{}）", rule.name, stats.summary()),
    )?;

    Ok(outcome(
        rule,
        "restored",
        if has_content {
            format!("已恢复：{} 已搬回 {}", stats.summary(), r.source.display())
        } else {
            format!("已拆掉链接并重建空目录：{}", r.source.display())
        },
        Some(stats),
        started,
        false,
    ))
}

/// 只重建链接，不搬运数据。用于「目标目录已就绪，但链接被软件更新弄丢了」。
pub fn relink_rule(store: &Store, rule: &Rule) -> TdResult<Outcome> {
    let started = Instant::now();
    let r = resolve(store, rule)?;
    let lt = rule_link_type(rule)?;
    let state = detect_link(&r.source, Some(&r.target));

    if state.is_managed_link() {
        return Ok(outcome(
            rule,
            "already_ok",
            "链接已存在，无需重建".into(),
            None,
            started,
            false,
        ));
    }
    // 先做「会不会破坏数据」的判定，再谈目标目录在不在。
    // 顺序反过来会给出误导性错误（"目标不存在"，而真正的问题是"源目录里有数据"）。
    if matches!(state, LinkState::PlainDir) && dir_has_content(&r.source) {
        return Err(TdError::SafetyBlocked(format!(
            "源是含内容的真实目录（{}），不能直接覆盖成链接。请改用 apply（会先搬运/合并）。",
            r.source.display()
        )));
    }
    if !r.target.exists() {
        return Err(TdError::InvalidPath(format!(
            "目标目录不存在，无法只重建链接：{}",
            r.target.display()
        )));
    }
    // 源是空壳目录：清掉再建链
    if matches!(state, LinkState::PlainDir) {
        std::fs::remove_dir(&util::to_verbatim(&r.source))
            .map_err(|e| crate::error::io_ctx(e, &r.source, "清理空源目录"))?;
    }
    create_dir_link(&r.source, &r.target, lt)?;
    store.set_rule_state(
        rule.id,
        if dir_has_content(&r.target) {
            "link_ok"
        } else {
            "link_ok_empty"
        },
        None,
    )?;
    store.log_event(
        Some(rule.id),
        "info",
        "relinked",
        &format!("{}：链接已重建 → {}", rule.name, r.target.display()),
    )?;
    Ok(outcome(
        rule,
        "relinked",
        format!("链接已重建：{} → {}", r.source.display(), r.target.display()),
        None,
        started,
        false,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::{NewRule, Store};
    use std::fs;

    struct Env {
        base: PathBuf,
        store: Store,
        rule_id: i64,
        source: PathBuf,
        target: PathBuf,
    }

    fn noop(_: Progress) {}

    /// 构造一个「RAM 盘就是本地某个目录」的测试环境。
    fn setup(tag: &str) -> Env {
        let base = std::env::temp_dir().join(format!("td_engine_{tag}_{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        let source = base.join("cache");
        let ram = base.join("ram");
        fs::create_dir_all(&source).unwrap();
        fs::create_dir_all(&ram).unwrap();
        fs::write(source.join("a.txt"), b"aaa").unwrap();
        fs::create_dir_all(source.join("sub")).unwrap();
        fs::write(source.join("sub").join("b.bin"), vec![9u8; 2048]).unwrap();

        let store = Store::open_in_memory().unwrap();
        let root_id = store
            .insert_root("RAM", &ram.to_string_lossy(), true, 90000)
            .unwrap();
        let rule_id = store
            .insert_rule(&NewRule {
                name: "测试缓存".into(),
                category: "dev".into(),
                source_template: source.to_string_lossy().into_owned(),
                target_root_id: root_id,
                target_subpath: "dev\\test-cache".into(),
                link_type: "junction".into(),
                want_enabled: true,
                rebuild_on_boot: true,
                notes: None,
            })
            .unwrap();
        let target = ram.join("dev").join("test-cache");

        Env {
            base,
            store,
            rule_id,
            source,
            target,
        }
    }

    #[test]
    fn apply_then_status_then_restore() {
        let env = setup("roundtrip");
        let rule = env.store.get_rule(env.rule_id).unwrap();

        let st = rule_status(&env.store, &rule).unwrap();
        assert!(matches!(st.state, LinkState::PlainDir), "{:?}", st.state);

        // dry-run 不做任何改动
        let dry = apply_rule(
            &env.store,
            &rule,
            &ApplyOptions {
                dry_run: true,
                ..Default::default()
            },
            &mut noop,
        )
        .unwrap();
        assert!(dry.dry_run);
        assert!(env.source.join("a.txt").exists(), "dry-run 不应动文件");

        // 正式应用
        let out = apply_rule(&env.store, &rule, &ApplyOptions::default(), &mut noop).unwrap();
        assert_eq!(out.action, "moved_and_linked");
        assert_eq!(out.stats.unwrap().files, 2);

        let st = rule_status(&env.store, &rule).unwrap();
        assert_eq!(st.badge, "✅");
        assert!(matches!(
            st.state,
            LinkState::LinkOk {
                target_state: TargetState::HasContent,
                ..
            }
        ));
        assert_eq!(fs::read(env.source.join("a.txt")).unwrap(), b"aaa");
        assert_eq!(
            fs::read(env.target.join("sub").join("b.bin")).unwrap().len(),
            2048
        );

        // 幂等
        let again = apply_rule(&env.store, &rule, &ApplyOptions::default(), &mut noop).unwrap();
        assert_eq!(again.action, "already_ok");

        // 恢复
        let back = restore_rule(&env.store, &rule, &ApplyOptions::default(), &mut noop).unwrap();
        assert_eq!(back.action, "restored");
        let st = rule_status(&env.store, &rule).unwrap();
        assert!(matches!(st.state, LinkState::PlainDir), "{:?}", st.state);
        assert_eq!(fs::read(env.source.join("a.txt")).unwrap(), b"aaa");
        assert!(!env.target.exists(), "恢复后目标内容应被搬走");

        // 再恢复一次：幂等
        let again = restore_rule(&env.store, &rule, &ApplyOptions::default(), &mut noop).unwrap();
        assert_eq!(again.action, "already_plain");

        let _ = fs::remove_dir_all(&env.base);
    }

    #[test]
    fn apply_recovers_after_ram_loss() {
        let env = setup("ramloss");
        let rule = env.store.get_rule(env.rule_id).unwrap();
        apply_rule(&env.store, &rule, &ApplyOptions::default(), &mut noop).unwrap();

        // 模拟 RAM 盘重启：目标目录整个消失
        fs::remove_dir_all(&env.target).unwrap();
        let st = rule_status(&env.store, &rule).unwrap();
        assert!(matches!(st.state, LinkState::LinkBroken { .. }), "{:?}", st.state);
        assert_eq!(st.badge, "⚠️");

        let out = apply_rule(&env.store, &rule, &ApplyOptions::default(), &mut noop).unwrap();
        assert_eq!(out.action, "fixed_broken_target");
        let st = rule_status(&env.store, &rule).unwrap();
        assert!(matches!(
            st.state,
            LinkState::LinkOk {
                target_state: TargetState::Empty,
                ..
            }
        ));

        let _ = fs::remove_dir_all(&env.base);
    }

    #[test]
    fn merge_into_non_empty_target() {
        let env = setup("merge");
        let rule = env.store.get_rule(env.rule_id).unwrap();
        fs::create_dir_all(&env.target).unwrap();
        fs::write(env.target.join("old.txt"), b"old").unwrap();

        // 默认拒绝
        let err = apply_rule(&env.store, &rule, &ApplyOptions::default(), &mut noop).unwrap_err();
        assert!(matches!(err, TdError::SafetyBlocked(_)), "{err}");

        // 允许合并
        let out = apply_rule(
            &env.store,
            &rule,
            &ApplyOptions {
                allow_merge: true,
                ..Default::default()
            },
            &mut noop,
        )
        .unwrap();
        assert_eq!(out.action, "merged_and_linked");
        assert!(env.target.join("old.txt").exists(), "原目标文件应保留");
        assert_eq!(fs::read(env.source.join("a.txt")).unwrap(), b"aaa");
        assert!(matches!(
            rule_status(&env.store, &rule).unwrap().state,
            LinkState::LinkOk { .. }
        ));

        let _ = fs::remove_dir_all(&env.base);
    }

    #[test]
    fn restore_empty_target_creates_plain_dir() {
        let env = setup("restoreempty");
        let rule = env.store.get_rule(env.rule_id).unwrap();
        apply_rule(&env.store, &rule, &ApplyOptions::default(), &mut noop).unwrap();
        fs::remove_file(env.target.join("a.txt")).unwrap();
        fs::remove_file(env.target.join("sub").join("b.bin")).unwrap();
        fs::remove_dir(env.target.join("sub")).unwrap();

        let out = restore_rule(&env.store, &rule, &ApplyOptions::default(), &mut noop).unwrap();
        assert_eq!(out.action, "restored");
        assert!(env.source.is_dir());
        assert!(matches!(
            rule_status(&env.store, &rule).unwrap().state,
            LinkState::PlainDir
        ));
        let _ = fs::remove_dir_all(&env.base);
    }

    #[test]
    fn source_missing_creates_empty_link() {
        let env = setup("missing");
        let rule = env.store.get_rule(env.rule_id).unwrap();
        fs::remove_dir_all(&env.source).unwrap();

        let out = apply_rule(&env.store, &rule, &ApplyOptions::default(), &mut noop).unwrap();
        assert_eq!(out.action, "created_and_linked");
        assert!(matches!(
            rule_status(&env.store, &rule).unwrap().state,
            LinkState::LinkOk {
                target_state: TargetState::Empty,
                ..
            }
        ));
        let _ = fs::remove_dir_all(&env.base);
    }

    #[test]
    fn relink_refuses_contentful_plain_dir() {
        let env = setup("relink");
        let rule = env.store.get_rule(env.rule_id).unwrap();
        // 目标就绪，但源是含内容的真实目录 → 必须拒绝，绝不能把内容盖掉
        fs::create_dir_all(&env.target).unwrap();
        let err = relink_rule(&env.store, &rule).unwrap_err();
        assert!(matches!(err, TdError::SafetyBlocked(_)), "{err}");
        assert!(env.source.join("a.txt").exists());
        let _ = fs::remove_dir_all(&env.base);
    }

    #[test]
    fn relink_requires_existing_target() {
        let env = setup("relinknotarget");
        let rule = env.store.get_rule(env.rule_id).unwrap();
        // 源是空壳目录 + 目标不存在 → 报"目标不存在"
        fs::remove_file(env.source.join("a.txt")).unwrap();
        fs::remove_file(env.source.join("sub").join("b.bin")).unwrap();
        fs::remove_dir(env.source.join("sub")).unwrap();
        let err = relink_rule(&env.store, &rule).unwrap_err();
        assert!(matches!(err, TdError::InvalidPath(_)), "{err}");
        let _ = fs::remove_dir_all(&env.base);
    }

    #[test]
    fn relink_works_on_empty_source_shell() {
        let env = setup("relinkok");
        let rule = env.store.get_rule(env.rule_id).unwrap();
        // 源被软件更新清空成空目录，目标已就绪 → 应该能直接重链
        fs::create_dir_all(&env.target).unwrap();
        fs::remove_file(env.source.join("a.txt")).unwrap();
        fs::remove_file(env.source.join("sub").join("b.bin")).unwrap();
        fs::remove_dir(env.source.join("sub")).unwrap();

        let out = relink_rule(&env.store, &rule).unwrap();
        assert_eq!(out.action, "relinked");
        assert!(matches!(
            rule_status(&env.store, &rule).unwrap().state,
            LinkState::LinkOk { .. }
        ));
        let _ = fs::remove_dir_all(&env.base);
    }

    #[test]
    fn rejected_apply_does_not_touch_anything() {
        let env = setup("reject");
        // 把目标根指向一个不存在的盘
        env.store
            .update_root(1, "RAM", "Z:\\NopeNotExist", true, 90000)
            .unwrap();
        let rule = env.store.get_rule(env.rule_id).unwrap();
        assert!(apply_rule(&env.store, &rule, &ApplyOptions::default(), &mut noop).is_err());
        assert!(env.source.join("a.txt").exists());
        let _ = fs::remove_dir_all(&env.base);
    }

    #[test]
    fn state_keys_are_stable() {
        assert_eq!(state_key(&LinkState::PlainDir), "plain_dir");
        assert_eq!(state_key(&LinkState::SourceMissing), "source_missing");
        assert_eq!(
            state_key(&LinkState::LinkOk {
                target: PathBuf::from("R:\\x"),
                link_type: LinkType::Junction,
                target_state: TargetState::Empty,
                target_mismatch: false,
            }),
            "link_ok_empty"
        );
    }
}
