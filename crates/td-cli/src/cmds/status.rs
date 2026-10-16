//! 体检与观测：doctor / status / locks / volume / events / ops。

use std::collections::HashMap;
use std::path::PathBuf;

use td_core::boot::{probe_volume, wait_for_volume};
use td_core::engine;
use td_core::fsops::occupancy::{scan_locks, LockScanOptions};
use td_core::link::detect::LinkState;
use td_core::paths;
use td_core::store::Store;
use td_core::{TdError, TdResult};

use crate::output::print_status_line;
use crate::parse::Parsed;

pub(crate) fn cmd_version() -> TdResult<()> {
    println!("TempDirector {}", env!("CARGO_PKG_VERSION"));
    println!("数据根可用环境变量 TEMPDIRECTOR_DATA_DIR 覆盖");
    Ok(())
}

pub(crate) fn cmd_doctor(store: &Store) -> TdResult<()> {
    println!("== 环境 ==");
    println!("数据目录        {}", paths::data_dir().display());
    println!("数据库          {}", paths::db_path().display());
    println!("schema 版本     v{}", store.user_version()?);
    println!("平台            {}", std::env::consts::OS);
    println!(
        "进程身份        {}",
        if td_core::elevate::is_elevated() {
            "管理员"
        } else {
            "普通用户"
        }
    );
    println!();
    println!("== 链接能力自检 ==");
    match junction_selftest() {
        Ok(note) => println!("✅ {note}"),
        Err(e) => println!("❌ {e}"),
    }
    println!();

    println!("== 目标根 ==");
    let roots = store.list_roots()?;
    if roots.is_empty() {
        println!("（未配置，用 `roots add` 添加）");
    }
    for r in &roots {
        let (p, unknown) = r.resolved_path();
        let (state, msg) = probe_volume(&p);
        let icon = match state {
            "ready" => "✅",
            "missing" => "❌",
            _ => "⚠️",
        };
        println!(
            "{icon} #{} {}  {}  [{state}] {msg}{}",
            r.id,
            r.label,
            r.path,
            if unknown.is_empty() {
                String::new()
            } else {
                format!("  未解析变量：{}", unknown.join("、"))
            }
        );
    }
    println!();

    println!("== 规则概况 ==");
    let rules = store.list_rules()?;
    println!("共 {} 条", rules.len());
    let mut by_state: HashMap<String, usize> = HashMap::new();
    let mut needs = 0usize;
    for rule in &rules {
        let st = engine::rule_status(store, rule)?;
        *by_state
            .entry(engine::state_key(&st.state).to_string())
            .or_default() += 1;
        if st.needs_attention {
            needs += 1;
        }
    }
    for (k, v) in by_state.iter() {
        println!("  {:<20} {v}", k);
    }
    println!("  需要处理：{needs}");
    println!();

    let open = store.list_open_ops()?;
    println!("== 待修复（上次中断的操作）==");
    if open.is_empty() {
        println!("无");
    } else {
        for op in open {
            println!(
                "  #{} rule={:?} {} 阶段={} 错误={}",
                op.id,
                op.rule_id,
                op.kind,
                op.phase,
                op.error.unwrap_or_else(|| "（无）".into())
            );
        }
    }
    Ok(())
}

/// 用临时目录做一次真实的 Junction 建/删，确认这台机器确实支持。
fn junction_selftest() -> TdResult<String> {
    let base = std::env::temp_dir().join(format!("td_selftest_{}", std::process::id()));
    let target = base.join("target");
    let link = base.join("link");
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(&target)?;

    let result = (|| -> TdResult<String> {
        td_core::link::create_dir_link(&link, &target, td_core::link::LinkType::Junction)?;
        let state = td_core::link::detect_link(&link, Some(&target));
        let note = match &state {
            LinkState::LinkOk { .. } => "目录 Junction 建立与识别正常（无需管理员权限）".to_string(),
            other => format!("Junction 建好了但识别异常：{}", other.label_zh()),
        };
        td_core::link::remove_dir_link(&link)?;
        Ok(note)
    })();

    let _ = std::fs::remove_dir_all(&base);
    result
}

pub(crate) fn cmd_status(store: &Store, p: &Parsed) -> TdResult<()> {
    let category = p.value("category");
    let search = p.value("search");
    let enabled_only = p.has("enabled") && !p.has("all");
    let persist = !p.has("no-persist");

    let list = engine::status_all(store, category, enabled_only, search, persist)?;
    if p.json() {
        println!("{}", serde_json::to_string_pretty(&list)?);
        return Ok(());
    }
    if list.is_empty() {
        println!("（无匹配规则）");
        return Ok(());
    }

    let mut counts: HashMap<&'static str, usize> = HashMap::new();
    for st in &list {
        *counts.entry(st.badge).or_default() += 1;
    }
    println!(
        "共 {} 条    ✅ {}   ⚠️ {}   ❌ {}   其他 {}",
        list.len(),
        counts.get("✅").copied().unwrap_or(0),
        counts.get("⚠️").copied().unwrap_or(0),
        counts.get("❌").copied().unwrap_or(0),
        list.len()
            - counts.get("✅").copied().unwrap_or(0)
            - counts.get("⚠️").copied().unwrap_or(0)
            - counts.get("❌").copied().unwrap_or(0)
    );
    println!();
    for (i, st) in list.iter().enumerate() {
        print_status_line(i, st);
    }
    Ok(())
}

pub(crate) fn cmd_locks(store: &Store, p: &Parsed) -> TdResult<()> {
    let target = p.required(0, "<规则id | 目录路径>")?;
    let dir: PathBuf = match target.parse::<i64>() {
        Ok(id) => {
            let rule = store.get_rule(id)?;
            let r = engine::resolve(store, &rule)?;
            r.source
        }
        Err(_) => PathBuf::from(&target),
    };
    if !dir.is_dir() {
        return Err(TdError::InvalidPath(format!(
            "不是目录：{}",
            dir.display()
        )));
    }
    println!("扫描占用：{}", dir.display());
    let report = scan_locks(&dir, &LockScanOptions::default())?;
    if p.json() {
        println!("{}", serde_json::to_string_pretty(&report)?);
        return Ok(());
    }
    println!("扫描 {} 个文件，耗时 {}ms", report.scanned, report.elapsed_ms);
    if report.is_clear() {
        println!("✅ 未发现占用，可以安全搬运");
        return Ok(());
    }
    println!("⚠️ {}", report.summary());
    for b in &report.blockers {
        println!("     占用者：{}", b.describe());
    }
    if !report.locked_files.is_empty() {
        println!("     被占用文件（最多显示 5 个）：");
        for f in report.locked_files.iter().take(5) {
            println!("       {}", f.display());
        }
    }
    if !report.denied_files.is_empty() {
        println!("     无权限文件（最多显示 5 个）：");
        for f in report.denied_files.iter().take(5) {
            println!("       {}", f.display());
        }
    }
    Ok(())
}

pub(crate) fn cmd_volume(p: &Parsed) -> TdResult<()> {
    let target = p.required(0, "<盘符或目录，如 R: 或 R:\\Redir>")?;
    let path = PathBuf::from(&target);
    let timeout = p
        .value("timeout")
        .map(|v| v.parse::<u64>())
        .transpose()
        .map_err(|_| TdError::Other("--timeout 必须是整数毫秒".into()))?
        .unwrap_or(0);
    if timeout == 0 {
        let (state, msg) = probe_volume(&path);
        println!("{state}: {msg}");
        return Ok(());
    }
    println!("等待 {target} 就绪（上限 {timeout}ms）…");
    let waited = wait_for_volume(
        &path,
        std::time::Duration::from_millis(timeout),
        std::time::Duration::from_millis(200),
    )?;
    println!("✅ 就绪，实际等待 {}ms", waited.as_millis());
    Ok(())
}

pub(crate) fn cmd_events(store: &Store, p: &Parsed) -> TdResult<()> {
    let limit = p
        .value("limit")
        .map(|v| v.parse::<i64>())
        .transpose()
        .map_err(|_| TdError::Other("--limit 必须是整数".into()))?
        .unwrap_or(30);
    for e in store.recent_events(limit)? {
        println!(
            "{} [{}] {:<20} {}",
            e.at,
            e.level,
            e.kind,
            e.message
        );
    }
    Ok(())
}

pub(crate) fn cmd_ops(store: &Store, p: &Parsed) -> TdResult<()> {
    let limit = p
        .value("limit")
        .map(|v| v.parse::<i64>())
        .transpose()
        .map_err(|_| TdError::Other("--limit 必须是整数".into()))?
        .unwrap_or(20);
    let open = store.list_open_ops()?;
    if !open.is_empty() {
        println!("⚠️ 有 {} 条待修复的操作流水：", open.len());
        for op in &open {
            println!(
                "   #{} rule={:?} kind={} phase={} err={}",
                op.id,
                op.rule_id,
                op.kind,
                op.phase,
                op.error.clone().unwrap_or_else(|| "（无）".into())
            );
        }
        println!();
    }
    for op in store.recent_ops(limit)? {
        println!(
            "{} #{:<4} {:<10} {:<12} files={} bytes={} {}{}",
            op.started_at,
            op.id,
            op.kind,
            op.phase,
            op.file_count.unwrap_or(0),
            op.bytes.unwrap_or(0),
            op.source_path.clone().unwrap_or_default(),
            op.error
                .as_ref()
                .map(|e| format!("  ERR: {e}"))
                .unwrap_or_default()
        );
    }
    Ok(())
}