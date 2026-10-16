//! 应用：把源目录变成指向 RAM 盘的链接，失败必回滚。

use std::time::Instant;

use serde::Serialize;

use crate::error::{TdError, TdResult};
use crate::fsops::mover::{self, MoveReport, Progress};
use crate::fsops::occupancy::{self, LockScanOptions};
use crate::fsops::verify::{self, TreeStats, VerifyMode};
use crate::i18n::{t, tf};
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
                t("apply.already_ok").into(),
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
                t("apply.already_ok_empty").into(),
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
            return Err(TdError::Other(tf("apply.mismatch", &[
                ("actual", &target.display().to_string()),
                ("target", &r.target.display().to_string()),
            ])));
        }
        LinkState::ForeignReparse { tag } => {
            return Err(TdError::SafetyBlocked(tf("apply.foreign", &[
                ("tag", &format!("{tag:#010x}")),
                ("path", &r.source.display().to_string()),
            ])));
        }
        LinkState::PlainFile => {
            return Err(TdError::InvalidPath(tf("apply.is_file", &[("path", &r.source.display().to_string())])));
        }
        LinkState::AccessDenied => {
            return Err(TdError::ElevationRequired(tf("apply.access_denied", &[("path", &r.source.display().to_string())])));
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
                    t("apply.dry_rebuild_target").into(),
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
                &tf("apply.target_rebuilt", &[("target", &r.target.display().to_string())]),
            )?;
            return Ok(outcome(
                rule,
                "fixed_broken_target",
                tf("apply.target_rebuilt_done", &[("target", &r.target.display().to_string())]),
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
                    t("apply.dry_create_link").into(),
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
                &tf("apply.linked_no_source", &[
                    ("created", if created { t("apply.created") } else { t("apply.exists") }),
                ]),
            )?;
            return Ok(outcome(
                rule,
                "created_and_linked",
                tf("apply.linked_no_source_done", &[("target", &r.target.display().to_string())]),
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
            return Err(TdError::Other(tf("apply.contradiction", &[("path", &r.source.display().to_string())])));
        }
        LinkState::PlainDir => { /* 走下面的搬运流程 */ }
    }

    // ---- 需要搬运 ----
    let src_stats = verify::measure(&r.source)?;
    let target_non_empty = dir_has_content(&r.target);

    if target_non_empty && !opts.allow_merge {
        return Err(TdError::SafetyBlocked(tf("apply.target_non_empty", &[("path", &r.target.display().to_string())])));
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
            return Err(TdError::Busy(tf("apply.source_busy", &[("summary", &report.summary())])));
        }
    }

    if opts.dry_run {
        let action = if target_non_empty {
            "merged_and_linked"
        } else {
            "moved_and_linked"
        };
        let what = if target_non_empty {
            t("apply.dry_merge").to_string()
        } else {
            t("apply.dry_move").to_string()
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
            note = tf("apply.merged", &[
                ("n", &(mr.copied + mr.overwritten).to_string()),
                ("over", &mr.overwritten.to_string()),
                ("skip", &mr.skipped.to_string()),
            ]);
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
                    Some(&tf("apply.rollback_ok", &[("e", &e.to_string()), ("rb", &rb)])),
                );
                let _ = store.set_rule_state(rule.id, "rollback", Some(&e.to_string()));
                let _ = store.log_event(
                    Some(rule.id),
                    "error",
                    "apply_failed",
                    &tf("apply.rollback_log", &[("name", &rule.name), ("e", &e.to_string()), ("rb", &rb)]),
                );
            }
            Err(rb) => {
                // 回滚也失败 → 留一条非终态流水，下次启动作为「待人工处理」
                let _ = store.op_phase(
                    op,
                    "needs_manual",
                    Some(&tf("apply.rollback_fail", &[("e", &e.to_string()), ("rb", &rb.to_string())])),
                );
                let _ = store.log_event(
                    Some(rule.id),
                    "error",
                    "needs_manual",
                    &tf("apply.rollback_fail_log", &[
                        ("name", &rule.name),
                        ("e", &e.to_string()),
                        ("rb", &rb.to_string()),
                        ("src", &r.source.display().to_string()),
                        ("tgt", &r.target.display().to_string()),
                    ]),
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
        &tf("apply.applied_log", &[("name", &rule.name), ("note", &note)]),
    )?;

    let mut o = outcome(
        rule,
        action,
        tf("apply.done_note", &[
            ("note", &note),
            ("src", &r.source.display().to_string()),
            ("tgt", &r.target.display().to_string()),
        ]),
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
            return Ok(t("apply.rb_merged").into());
        }
        return Ok(t("apply.rb_source_exists").into());
    }

    if !dir_has_content(&r.target) {
        return Ok(t("apply.rb_target_empty").into());
    }
    if r.source.exists() {
        return Ok(t("apply.rb_source_exists2").into());
    }

    let mut noop = |_: Progress| {};
    let mr = mover::move_tree(&r.target, &r.source, VerifyMode::CountAndSize, &mut noop)
        .map_err(|e| e.to_string())?;
    Ok(tf("apply.rb_moved_back", &[("summary", &mr.stats.summary())]))
}