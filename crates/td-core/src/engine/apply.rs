//! 应用：把源目录变成指向 RAM 盘的链接，失败必回滚。

use std::time::Instant;

use serde::Serialize;

use crate::error::{TdError, TdResult};
use crate::fsops::mover::{self, MoveReport, Progress};
use crate::fsops::occupancy::{self, LockScanOptions};
use crate::fsops::verify::{self, TreeStats, VerifyMode};
use crate::link::create::{create_dir_link, ensure_dir, remove_dir_link};
use crate::link::detect::{detect_link, LinkState, TargetState};
use crate::store::{Rule, Store};

use super::resolve::{dir_has_content, resolve, rule_link_type, validate_ready, Resolved};

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

pub(super) fn outcome(
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