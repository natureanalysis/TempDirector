//! 文件夹清理用例（清空 / 按时间删除）：解析目标、预演、执行。

use std::path::PathBuf;

use crate::engine;
use crate::error::{TdError, TdResult};
use crate::fsops::clean;
use crate::fsops::exclude::Excludes;
use crate::fsops::mover::Progress;
use crate::safety;
use crate::store::Store;
use crate::util;

use super::view::{CleanPreviewView, CleanResultView, CleanTarget};

/// 解析出真正要清理的目录，以及规则上配置的清理排除模式。
///
/// 规则：源目录若已是链接（联接/符号链接），真实内容在 RAM 盘目标上，清目标；
/// 否则（普通目录 / 未链接）清源。文件夹：展开 `%VAR%` 后过一遍系统目录黑名单。
/// 排除模式只来自规则（任意文件夹清理不带排除）。
fn resolve_clean_dir(
    store: &Store,
    target: &CleanTarget,
) -> TdResult<(PathBuf, String, Excludes)> {
    match target {
        CleanTarget::Rule { id } => {
            let rule = store.get_rule(*id)?;
            let dir = engine::clean_dir_for_rule(store, &rule)?;
            let excludes = Excludes::new(&rule.excludes)?;
            Ok((dir, rule.name, excludes))
        }
        CleanTarget::Folder { path } => {
            let (expanded, _unknown) = util::expand_path(path);
            let pf = safety::preflight_source(&expanded);
            if pf.is_blocked() {
                return Err(TdError::SafetyBlocked(pf.blocking.join("；")));
            }
            let label = expanded.display().to_string();
            Ok((expanded, label, Excludes::default()))
        }
    }
}

/// 预演：只算不删。目录不存在时返回空计划（而不是报错——清理一个还没内容的缓存是常见的）。
pub fn clean_preview(
    store: &Store,
    target: &CleanTarget,
    policy: &clean::CleanPolicy,
) -> TdResult<CleanPreviewView> {
    let (dir, label, excludes) = resolve_clean_dir(store, target)?;
    policy.validate()?;
    let plan = if dir.is_dir() {
        clean::plan(&dir, policy, &excludes)?
    } else {
        clean::CleanPlan::default()
    };
    let summary = plan.summary();
    let bytes_text = util::human_bytes(plan.bytes);
    Ok(CleanPreviewView {
        dir: dir.display().to_string(),
        label,
        files: plan.files,
        bytes: plan.bytes,
        bytes_text,
        links_skipped: plan.links_skipped,
        excluded_skipped: plan.excluded_skipped,
        sample: plan.sample,
        summary,
    })
}

/// 执行清理。`dry_run` 走预演分支不写盘。完成后落一条事件流水。
pub fn clean_run(
    store: &Store,
    target: &CleanTarget,
    policy: &clean::CleanPolicy,
    dry_run: bool,
    on_progress: &mut dyn FnMut(&Progress),
) -> TdResult<CleanResultView> {
    let (dir, label, excludes) = resolve_clean_dir(store, target)?;
    policy.validate()?;
    let report = if dir.is_dir() {
        let mut sink = |p: Progress| on_progress(&p);
        clean::run(&dir, policy, &excludes, dry_run, &mut sink)?
    } else {
        clean::CleanReport::default()
    };
    let summary = report.summary();
    let deleted_bytes_text = util::human_bytes(report.deleted_bytes);
    if !dry_run && report.deleted_files > 0 {
        store.log_event(
            match target {
                CleanTarget::Rule { id } => Some(*id),
                CleanTarget::Folder { .. } => None,
            },
            "info",
            "clean",
            &format!("清理「{label}」：{summary}"),
        )?;
    }
    Ok(CleanResultView {
        dir: dir.display().to_string(),
        label,
        deleted_files: report.deleted_files,
        deleted_bytes: report.deleted_bytes,
        deleted_bytes_text,
        dirs_pruned: report.dirs_pruned,
        skipped: report.skipped,
        skip_examples: report.skip_examples,
        links_skipped: report.links_skipped,
        excluded_skipped: report.excluded_skipped,
        summary,
    })
}