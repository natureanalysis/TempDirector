//! `clean` 子命令：文件夹清理。默认只预演，`--yes` 才真正删除（破坏性操作两步走）。

use std::path::PathBuf;

use td_core::engine;
use td_core::fsops::clean::{self, CleanPolicy};
use td_core::store::Store;
use td_core::{TdError, TdResult};

use crate::parse::Parsed;
use crate::timefmt::{ago_text, human_duration, local_time_of_day_secs, now_utc_secs, parse_when};

/// 从参数装配四策略之一（互斥）。
fn build_clean_policy(p: &Parsed, now_utc: i64, tod_secs: i64) -> TdResult<CleanPolicy> {
    let empty = p.has("empty");
    let yesterday = p.has("yesterday");
    let older = p.value("older-than");
    let from = p.value("from");
    let to = p.value("to");
    let range = from.is_some() || to.is_some();

    let chosen = [empty, yesterday, older.is_some(), range]
        .iter()
        .filter(|b| **b)
        .count();
    if chosen == 0 {
        return Err(TdError::Other(
            "请指定清理策略：--empty / --yesterday / --older-than <时长> / --from <起> --to <止>"
                .into(),
        ));
    }
    if chosen > 1 {
        return Err(TdError::Other("清理策略四选一，不能同时指定多个".into()));
    }
    if empty {
        return Ok(CleanPolicy::Empty);
    }
    if yesterday {
        return Ok(CleanPolicy::Before {
            cutoff: now_utc - tod_secs,
        });
    }
    if let Some(v) = older {
        return Ok(CleanPolicy::Before {
            cutoff: parse_when(v, now_utc, tod_secs)?,
        });
    }
    let f = from.ok_or_else(|| TdError::Other("--from 与 --to 需成对给出".into()))?;
    let t = to.ok_or_else(|| TdError::Other("--from 与 --to 需成对给出".into()))?;
    Ok(CleanPolicy::Between {
        from: parse_when(f, now_utc, tod_secs)?,
        to: parse_when(t, now_utc, tod_secs)?,
    })
}

/// 把策略渲染成中文（相对时长口径，供预演展示）。
fn describe_policy(policy: &CleanPolicy, now_utc: i64) -> String {
    match policy {
        CleanPolicy::Empty => "清空全部文件".into(),
        CleanPolicy::Before { cutoff } => {
            format!("删除 {} 之前修改的文件", human_duration(now_utc - *cutoff))
        }
        CleanPolicy::Between { from, to } => format!(
            "删除 {} ~ {} 之间修改的文件（左闭右开）",
            ago_text(now_utc - *from),
            ago_text(now_utc - *to)
        ),
    }
}

/// 解析清理目标：数字当规则 id（走 clean_dir_for_rule），否则当目录路径（过黑名单）。
fn resolve_clean_target(store: &Store, target: &str) -> TdResult<(PathBuf, String, Option<i64>)> {
    if let Ok(id) = target.parse::<i64>() {
        let rule = store.get_rule(id)?;
        let dir = engine::clean_dir_for_rule(store, &rule)?;
        Ok((dir, rule.name, Some(id)))
    } else {
        let (expanded, _u) = td_core::util::expand_path(target);
        let pf = td_core::safety::preflight_source(&expanded);
        if pf.is_blocked() {
            return Err(TdError::SafetyBlocked(pf.blocking.join("；")));
        }
        let label = expanded.display().to_string();
        Ok((expanded, label, None))
    }
}

pub(crate) fn cmd_clean(store: &Store, p: &Parsed) -> TdResult<()> {
    let target = p.required(0, "<规则id | 目录路径>")?;
    let now_utc = now_utc_secs();
    let tod = local_time_of_day_secs() as i64;
    let policy = build_clean_policy(p, now_utc, tod)?;
    let (dir, label, rule_id) = resolve_clean_target(store, &target)?;

    if !dir.is_dir() {
        println!("目录不存在，无需清理：{}", dir.display());
        return Ok(());
    }

    // 默认预演；只有显式 --yes 才真正删除（破坏性操作，两步走）。
    let execute = p.has("yes");

    if !execute {
        let plan = clean::plan(&dir, &policy)?;
        if p.json() {
            println!(
                "{}",
                serde_json::json!({
                    "dir": dir.display().to_string(),
                    "label": label,
                    "policy": policy,
                    "dry_run": true,
                    "plan": plan,
                })
            );
            return Ok(());
        }
        println!("目标：{label}");
        println!("目录：{}", dir.display());
        println!("策略：{}", describe_policy(&policy, now_utc));
        println!();
        if plan.is_empty() {
            println!("✅ 没有符合条件的文件，无需清理。");
            return Ok(());
        }
        println!("将删除：{}", plan.summary());
        if plan.links_skipped > 0 {
            println!("（另有 {} 个链接不会被删除或进入）", plan.links_skipped);
        }
        for s in &plan.sample {
            println!(
                "  · {}  {}  {}",
                s.path,
                td_core::util::human_bytes(s.bytes),
                ago_text(now_utc - s.mtime)
            );
        }
        if plan.files > plan.sample.len() as u64 {
            println!(
                "  …（仅列出前 {} 条，共 {} 个）",
                plan.sample.len(),
                plan.files
            );
        }
        println!();
        println!("（这是预演，未删除任何文件。确认无误后加 --yes 执行）");
        return Ok(());
    }

    let quiet = p.json();
    let mut sink = |prog: td_core::fsops::mover::Progress| {
        if quiet {
            return;
        }
        if let td_core::fsops::mover::Progress::Deleting { files } = prog {
            print!("\r  已删除 {files} 个文件…");
            use std::io::Write;
            let _ = std::io::stdout().flush();
        }
    };
    let report = clean::run(&dir, &policy, false, &mut sink)?;
    if !quiet {
        println!();
    }
    let _ = store.log_event(
        rule_id,
        "info",
        "clean",
        &format!("CLI 清理「{label}」：{}", report.summary()),
    );
    if p.json() {
        println!(
            "{}",
            serde_json::json!({
                "dir": dir.display().to_string(),
                "label": label,
                "policy": policy,
                "dry_run": false,
                "report": report,
            })
        );
    } else {
        println!("✅ {}", report.summary());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: i64 = 1_700_000_000;
    const TOD: i64 = 3600 * 10 + 200; // 当天已过 10:03:20

    fn parsed(args: &[&str]) -> Parsed {
        Parsed::parse(&args.iter().map(|s| s.to_string()).collect::<Vec<_>>())
    }

    #[test]
    fn build_policy_four_modes() {
        assert_eq!(
            build_clean_policy(&parsed(&["--empty"]), NOW, TOD).unwrap(),
            CleanPolicy::Empty
        );
        assert_eq!(
            build_clean_policy(&parsed(&["--yesterday"]), NOW, TOD).unwrap(),
            CleanPolicy::Before { cutoff: NOW - TOD }
        );
        assert_eq!(
            build_clean_policy(&parsed(&["--older-than", "1h"]), NOW, TOD).unwrap(),
            CleanPolicy::Before { cutoff: NOW - 3600 }
        );
        assert_eq!(
            build_clean_policy(&parsed(&["--from", "2d", "--to", "1h"]), NOW, TOD).unwrap(),
            CleanPolicy::Between {
                from: NOW - 2 * 86400,
                to: NOW - 3600
            }
        );
    }

    #[test]
    fn build_policy_rejects_bad_combos() {
        assert!(build_clean_policy(&parsed(&[]), NOW, TOD).is_err(), "无策略应报错");
        assert!(
            build_clean_policy(&parsed(&["--empty", "--yesterday"]), NOW, TOD).is_err(),
            "多策略应报错"
        );
        assert!(
            build_clean_policy(&parsed(&["--from", "2d"]), NOW, TOD).is_err(),
            "--from 缺 --to 应报错"
        );
    }
}