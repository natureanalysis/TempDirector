//! `clean` 子命令：文件夹清理。默认只预演，`--yes` 才真正删除（破坏性操作两步走）。

use std::path::Path;

use td_core::fsops::clean::CleanPolicy;
use td_core::store::Store;
use td_core::usecase::{clean_preview, clean_run, CleanTarget};
use td_core::{TdError, TdResult};

use crate::parse::Parsed;
use crate::timefmt::{ago_text, human_duration, local_time_of_day_secs, now_utc_secs, parse_when};

/// 从参数装配四策略之一（互斥）。`water sweep` 复用同一套策略参数。
pub(crate) fn build_clean_policy(p: &Parsed, now_utc: i64, tod_secs: i64) -> TdResult<CleanPolicy> {
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

/// 把策略渲染成中文（相对时长口径，供预演展示）。`water sweep` 复用。
pub(crate) fn describe_policy(policy: &CleanPolicy, now_utc: i64) -> String {
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

/// 解析清理目标：数字当规则 id（清它真正落地的目录，自动带上规则排除），
/// 否则当目录路径（展开 `%VAR%`、过黑名单都在 usecase 层）。
fn parse_clean_target(target: &str) -> CleanTarget {
    match target.parse::<i64>() {
        Ok(id) => CleanTarget::Rule { id },
        Err(_) => CleanTarget::Folder {
            path: target.to_string(),
        },
    }
}

pub(crate) fn cmd_clean(store: &Store, p: &Parsed) -> TdResult<()> {
    let target_str = p.required(0, "<规则id | 目录路径>")?;
    let now_utc = now_utc_secs();
    let tod = local_time_of_day_secs() as i64;
    let policy = build_clean_policy(p, now_utc, tod)?;
    let target = parse_clean_target(&target_str);

    // 默认预演；只有显式 --yes 才真正删除（破坏性操作，两步走）。
    if !p.has("yes") {
        let pv = clean_preview(store, &target, &policy)?;
        if p.json() {
            println!(
                "{}",
                serde_json::json!({ "policy": policy, "dry_run": true, "preview": pv })
            );
            return Ok(());
        }
        println!("目标：{}", pv.label);
        println!("目录：{}", pv.dir);
        println!("策略：{}", describe_policy(&policy, now_utc));
        println!();
        if !Path::new(&pv.dir).is_dir() {
            println!("目录不存在，无需清理。");
            return Ok(());
        }
        if pv.files == 0 {
            println!("✅ 没有符合条件的文件，无需清理。");
            return Ok(());
        }
        println!("将删除：{}", pv.summary);
        if pv.links_skipped > 0 {
            println!("（另有 {} 个链接不会被删除或进入）", pv.links_skipped);
        }
        if pv.excluded_skipped > 0 {
            println!("（另有 {} 个条目因排除模式被跳过）", pv.excluded_skipped);
        }
        for s in &pv.sample {
            println!(
                "  · {}  {}  {}",
                s.path,
                td_core::util::human_bytes(s.bytes),
                ago_text(now_utc - s.mtime)
            );
        }
        if pv.files > pv.sample.len() as u64 {
            println!(
                "  …（仅列出前 {} 条，共 {} 个）",
                pv.sample.len(),
                pv.files
            );
        }
        println!();
        println!("（这是预演，未删除任何文件。确认无误后加 --yes 执行）");
        return Ok(());
    }

    let quiet = p.json();
    let mut sink = |prog: &td_core::fsops::mover::Progress| {
        if quiet {
            return;
        }
        if let td_core::fsops::mover::Progress::Deleting { files } = prog {
            print!("\r  已删除 {files} 个文件…");
            use std::io::Write;
            let _ = std::io::stdout().flush();
        }
    };
    // 事件流水由 usecase 层统一落库（原 CLI 自行记一条会重复）。
    let report = clean_run(store, &target, &policy, false, &mut sink)?;
    if !quiet {
        println!();
    }
    if p.json() {
        println!(
            "{}",
            serde_json::json!({ "policy": policy, "dry_run": false, "result": report })
        );
    } else {
        println!("✅ {}", report.summary);
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