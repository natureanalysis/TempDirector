//! 恢复：拆掉链接、把 RAM 盘内容搬回原位；以及只重建链接（relink）。

use std::time::Instant;

use crate::error::{TdError, TdResult};
use crate::fsops::mover::{self, Progress};
use crate::fsops::verify::TreeStats;
use crate::link::create::{create_dir_link, ensure_dir, remove_dir_link};
use crate::link::detect::{detect_link, LinkState};
use crate::store::{Rule, Store};
use crate::util;

use super::apply::{outcome, ApplyOptions, Outcome};
use super::resolve::{dir_has_content, resolve, rule_link_type};

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