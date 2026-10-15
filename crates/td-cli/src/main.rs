//! TempDirector 命令行工具。
//!
//! 这个二进制同时承担三个角色：
//! 1. **人用的管理界面**（M1 阶段的验证载体，M2 之后由 GUI 取代大部分场景）；
//! 2. **提权 worker**（`apply`/`restore` 在需要管理员权限时，GUI 会以 `runas` 方式
//!    重新拉起本程序执行同一条规则 —— 见 `docs/ARCHITECTURE.md` 的提权设计）；
//! 3. **开机重建的执行体**（`boot-rebuild`，由任务计划或后台服务调用）。
//!
//! 之所以把这些都放在 CLI 而不是 GUI 里：提权路径和开机路径都不应该拖上 WebView，
//! 攻击面更小、启动更快，也让"开机重建"可以在没有界面的会话里跑。

use std::collections::HashMap;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::{SystemTime, UNIX_EPOCH};

use td_core::boot::{probe_volume, run_boot_rebuild, wait_for_volume};
use td_core::engine::{self, apply_rule, restore_rule, ApplyOptions};
use td_core::fsops::clean::{self, CleanPolicy};
use td_core::fsops::occupancy::{scan_locks, LockScanOptions};
use td_core::fsops::verify::VerifyMode;
use td_core::link::detect::LinkState;
use td_core::paths;
use td_core::portable;
use td_core::store::{NewRule, Rule, Store, CATEGORIES};
use td_core::{TdError, TdResult};

// ---------------------------------------------------------------------------
// 极简参数解析（不引入 clap：本程序的参数面很窄，手写更可控也更少依赖）
// ---------------------------------------------------------------------------

#[derive(Debug, Default)]
struct Parsed {
    positional: Vec<String>,
    flags: HashMap<String, Option<String>>,
}

impl Parsed {
    fn parse(args: &[String]) -> Self {
        let mut out = Parsed::default();
        let mut i = 0usize;
        while i < args.len() {
            let a = &args[i];
            if let Some(rest) = a.strip_prefix("--") {
                if let Some((k, v)) = rest.split_once('=') {
                    out.flags.insert(k.to_string(), Some(v.to_string()));
                } else {
                    let next_is_value = args
                        .get(i + 1)
                        .map(|n| !n.starts_with("--"))
                        .unwrap_or(false);
                    if next_is_value {
                        out.flags
                            .insert(rest.to_string(), Some(args[i + 1].clone()));
                        i += 1;
                    } else {
                        out.flags.insert(rest.to_string(), None);
                    }
                }
            } else {
                out.positional.push(a.clone());
            }
            i += 1;
        }
        out
    }

    fn has(&self, name: &str) -> bool {
        self.flags.contains_key(name)
    }

    fn value(&self, name: &str) -> Option<&str> {
        self.flags.get(name).and_then(|v| v.as_deref())
    }

    /// 位置参数（可被 `--` 后的内容追加）。
    fn pos(&self, idx: usize) -> Option<&str> {
        self.positional.get(idx).map(|s| s.as_str())
    }

    fn required(&self, idx: usize, what: &str) -> TdResult<String> {
        self.pos(idx)
            .map(|s| s.to_string())
            .ok_or_else(|| TdError::Other(format!("缺少参数：{what}")))
    }

    /// 解析 id 列表（`rules enable 1 2 3`）或 `--all`。
    fn ids(&self, start: usize) -> TdResult<Vec<i64>> {
        let mut out = Vec::new();
        let mut i = start;
        while let Some(s) = self.pos(i) {
            out.push(
                s.parse::<i64>()
                    .map_err(|_| TdError::Other(format!("不是合法的规则 id：{s}")))?,
            );
            i += 1;
        }
        Ok(out)
    }

    fn json(&self) -> bool {
        self.has("json")
    }
}

// ---------------------------------------------------------------------------
// 输出
// ---------------------------------------------------------------------------

fn print_status_line(i: usize, st: &engine::RuleStatus) {
    println!(
        "{} #{:<4} [{}] {}",
        st.badge,
        st.rule_id,
        td_core::store::category_label(&st.category),
        st.name
    );
    println!("       源：{}", st.source);
    println!("       目标：{}  ({})", st.target, st.link_type);
    println!(
        "       状态：{}  开机重建：{}  期望开启：{}",
        st.label,
        if st.rebuild_on_boot { "是" } else { "否" },
        if st.want_enabled { "是" } else { "否" }
    );
    for w in &st.warnings {
        println!("       ⚠ {w}");
    }
    if i > 0 {
        println!();
    }
}

fn print_rule_row(rule: &Rule) {
    println!(
        "#{:<4} [{}] {}  →  {}",
        rule.id,
        td_core::store::category_label(&rule.category),
        rule.name,
        rule.target_subpath
    );
    println!(
        "       源：{}   链接：{}   开启：{}  开机重建：{}  需提权：{}",
        rule.source_template,
        rule.link_type,
        yn(rule.want_enabled),
        yn(rule.rebuild_on_boot),
        yn(rule.needs_elevation)
    );
    if let Some(n) = &rule.notes {
        println!("       备注：{n}");
    }
}

fn yn(v: bool) -> &'static str {
    if v {
        "是"
    } else {
        "否"
    }
}

fn print_outcome(o: &engine::Outcome) {
    let tag = if o.dry_run {
        "预演"
    } else {
        match o.action {
            "already_ok" | "already_plain" => "跳过",
            "moved_and_linked" | "merged_and_linked" | "created_and_linked" => "完成",
            "fixed_broken_target" => "修复",
            "restored" => "恢复",
            "relinked" => "重建",
            _ => "完成",
        }
    };
    println!(
        "  [{tag}] #{:<4} {:<20} {}",
        o.rule_id, o.name, o.message
    );
    if let Some(s) = o.stats {
        if s.files > 0 || s.bytes > 0 {
            println!(
                "         规模：{}，耗时 {}",
                s.summary(),
                td_core::util::human_dur_ms(o.elapsed_ms)
            );
        }
    }
}

// ---------------------------------------------------------------------------
// 子命令
// ---------------------------------------------------------------------------

fn cmd_version() -> TdResult<()> {
    println!("TempDirector {}", env!("CARGO_PKG_VERSION"));
    println!("数据根可用环境变量 TEMPDIRECTOR_DATA_DIR 覆盖");
    Ok(())
}

fn cmd_doctor(store: &Store) -> TdResult<()> {
    println!("== 环境 ==");
    println!("数据目录        {}", paths::data_dir().display());
    println!("数据库          {}", paths::db_path().display());
    println!("schema 版本     v{}", store.user_version()?);
    println!("平台            {}", std::env::consts::OS);
    println!(
        "进程身份        {}",
        if is_elevated() { "管理员" } else { "普通用户" }
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

fn cmd_roots(store: &Store, p: &Parsed) -> TdResult<()> {
    match p.pos(0).unwrap_or("list") {
        "list" => {
            let roots = store.list_roots()?;
            if roots.is_empty() {
                println!("（未配置，示例：roots add RAMDisk R: ）");
            }
            for r in roots {
                let (path, unknown) = r.resolved_path();
                let (state, msg) = probe_volume(&path);
                println!(
                    "#{:<3} {:<20} {:<20} 等待上限 {}ms  盘：{}  {}\n     {}",
                    r.id,
                    r.label,
                    r.path,
                    r.wait_timeout_ms,
                    if r.is_ramdisk { "RAM" } else { "普通" },
                    state,
                    msg
                );
                if !unknown.is_empty() {
                    println!("     未解析变量：{}", unknown.join("、"));
                }
            }
            Ok(())
        }
        "add" => {
            let label = p.required(1, "<label>")?;
            let path = p.required(2, "<path>")?;
            let is_ram = !p.has("hdd");
            let timeout = p
                .value("timeout")
                .map(|v| v.parse::<i64>())
                .transpose()
                .map_err(|_| TdError::Other("--timeout 必须是整数毫秒".into()))?
                .unwrap_or(90000);
            let id = store.insert_root(&label, &path, is_ram, timeout)?;
            println!("已添加目标根 #{id}: {label} → {path}");
            Ok(())
        }
        "rm" | "remove" | "delete" => {
            let id = p.required(1, "<id>")?.parse::<i64>().map_err(|_| {
                TdError::Other("目标根 id 必须是整数".into())
            })?;
            store.delete_root(id)?;
            println!("已删除目标根 #{id}");
            Ok(())
        }
        other => Err(TdError::Other(format!(
            "未知的 roots 子命令：{other}（可用：list / add / rm）"
        ))),
    }
}

fn cmd_rules(store: &Store, p: &Parsed) -> TdResult<()> {
    match p.pos(0).unwrap_or("list") {
        "list" => {
            let category = p.value("category");
            let search = p.value("search");
            let enabled_only = p.has("enabled");
            let rules = store.query_rules(category, enabled_only, search)?;
            if p.json() {
                println!("{}", serde_json::to_string_pretty(&rules)?);
                return Ok(());
            }
            if rules.is_empty() {
                println!("（无匹配规则）");
                return Ok(());
            }
            println!(
                "共 {} 条（分类：{}）",
                rules.len(),
                category.unwrap_or("全部")
            );
            println!();
            for (i, r) in rules.iter().enumerate() {
                print_rule_row(r);
                if i + 1 < rules.len() {
                    println!();
                }
            }
            Ok(())
        }
        "add" => {
            let name = p.required(1, "<名称>")?;
            let source = p.required(2, "<源路径模板，如 %LOCALAPPDATA%\\X\\Cache>")?;
            let subpath = p.required(3, "<目标子目录，如 browsers\\chrome>")?;

            let root_id = match p.value("root") {
                Some(v) => v
                    .parse::<i64>()
                    .map_err(|_| TdError::Other("--root 必须是整数 id".into()))?,
                None => {
                    let roots = store.list_roots()?;
                    match roots.len() {
                        0 => {
                            return Err(TdError::Other(
                                "还没有配置目标根，先执行：roots add RAMDisk R:".into(),
                            ))
                        }
                        1 => roots[0].id,
                        _ => {
                            let list = roots
                                .iter()
                                .map(|r| format!("#{} {}", r.id, r.label))
                                .collect::<Vec<_>>()
                                .join("、");
                            return Err(TdError::Other(format!(
                                "有多个目标根，请用 --root 指定：{list}"
                            )));
                        }
                    }
                }
            };

            let category = p.value("category").unwrap_or("other").to_string();
            if !CATEGORIES.contains(&category.as_str()) {
                return Err(TdError::Other(format!(
                    "未知分类 {category}，可用：{}",
                    CATEGORIES.join(" / ")
                )));
            }
            let link_type = p.value("link-type").unwrap_or("junction").to_string();
            if td_core::link::LinkType::parse(&link_type).is_none() {
                return Err(TdError::Other(
                    "--link-type 只能是 junction 或 symlink_dir".into(),
                ));
            }

            let nr = NewRule {
                name,
                category,
                source_template: source,
                target_root_id: root_id,
                target_subpath: subpath,
                link_type,
                want_enabled: p.has("enable"),
                rebuild_on_boot: !p.has("no-boot"),
                notes: p.value("notes").map(|s| s.to_string()),
            };
            let id = store.insert_rule(&nr)?;
            let rule = store.get_rule(id)?;
            println!("已添加规则 #{id}");
            print_rule_row(&rule);
            if rule.needs_elevation {
                println!("提示：该源路径位于受保护位置，应用时需要管理员权限");
            }
            Ok(())
        }
        "rm" | "remove" | "delete" => {
            for id in p.ids(1)? {
                store.delete_rule(id)?;
                println!("已删除规则 #{id}");
            }
            Ok(())
        }
        "enable" | "disable" => {
            let on = p.pos(0) == Some("enable");
            let ids = if p.has("all") {
                store.list_rules()?.into_iter().map(|r| r.id).collect()
            } else {
                p.ids(1)?
            };
            if ids.is_empty() {
                return Err(TdError::Other("请指定规则 id 或 --all".into()));
            }
            for id in ids {
                store.set_rule_enabled(id, on)?;
                println!("规则 #{id} 期望开关 → {}", yn(on));
            }
            Ok(())
        }
        other => Err(TdError::Other(format!(
            "未知的 rules 子命令：{other}（可用：list / add / rm / enable / disable）"
        ))),
    }
}

fn cmd_status(store: &Store, p: &Parsed) -> TdResult<()> {
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

fn cmd_locks(store: &Store, p: &Parsed) -> TdResult<()> {
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

/// 本地"当天已过多少秒"（时:分:秒）。用于把 UTC now 折算本地零点：
/// 本地零点 = utc_now − 当天已过秒数。绕开时区偏移与 DST 边界（OS 直接给本地墙上时间）。
#[cfg(windows)]
fn local_time_of_day_secs() -> u32 {
    use windows_sys::Win32::Foundation::SYSTEMTIME;
    use windows_sys::Win32::System::SystemInformation::GetLocalTime;
    let mut st: SYSTEMTIME = unsafe { std::mem::zeroed() };
    unsafe { GetLocalTime(&mut st) };
    st.wHour as u32 * 3600 + st.wMinute as u32 * 60 + st.wSecond as u32
}

#[cfg(not(windows))]
fn local_time_of_day_secs() -> u32 {
    0
}

fn now_utc_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// 把秒数转成"3 天 / 1 小时 / 2 分 / 45 秒"这样的大单位人话。
/// 不复用 `util::human_dur_ms`（它只到秒级，86400s 会显示成"86400.0s"）。
fn human_duration(secs: i64) -> String {
    if secs <= 0 {
        return "0 秒".into();
    }
    let d = secs / 86400;
    let h = (secs % 86400) / 3600;
    let m = (secs % 3600) / 60;
    let s = secs % 60;
    let mut parts: Vec<String> = Vec::new();
    if d > 0 {
        parts.push(format!("{d} 天"));
        if h > 0 && d < 30 {
            parts.push(format!("{h} 小时"));
        }
    } else if h > 0 {
        parts.push(format!("{h} 小时"));
        if m > 0 {
            parts.push(format!("{m} 分"));
        }
    } else if m > 0 {
        parts.push(format!("{m} 分"));
    } else {
        parts.push(format!("{s} 秒"));
    }
    parts.join(" ")
}

/// 距今多久以前（带"前"）。缓存清理关心的是新旧程度，不是绝对时刻，
/// 用相对时长既直观、又彻底绕开日历/时区数学（不引 chrono）。
fn ago_text(secs_ago: i64) -> String {
    if secs_ago <= 0 {
        return "现在".into();
    }
    format!("{}前", human_duration(secs_ago))
}

/// 解析时间点参数为 Unix 秒。接受：
/// - `today` / `yesterday`：本地零点 / 昨天零点；
/// - 纯数字：绝对 Unix 秒；
/// - 相对时长 `90s` / `30m` / `2h` / `7d`：距今多久以前。
fn parse_when(s: &str, now_utc: i64, tod_secs: i64) -> TdResult<i64> {
    let t = s.trim().to_ascii_lowercase();
    if t == "today" {
        return Ok(now_utc - tod_secs);
    }
    if t == "yesterday" {
        return Ok(now_utc - tod_secs - 86400);
    }
    if let Ok(secs) = t.parse::<i64>() {
        return Ok(secs);
    }
    // 数字前缀 + 字母单位。用 find+切片把字母**保留**在 unit 里
    // （split_once 会丢掉分隔符本身，"1h" 会被切成 ("1","")）。
    let idx = t.find(|c: char| c.is_alphabetic()).ok_or_else(|| {
        TdError::Other(format!(
            "无法理解的时间「{s}」（可用 today / yesterday / 90s / 30m / 2h / 7d / Unix秒）"
        ))
    })?;
    let (num, unit) = (&t[..idx], &t[idx..]);
    let n: i64 = num
        .parse()
        .map_err(|_| TdError::Other(format!("时长的数字部分非法：{s}")))?;
    let mult = match unit {
        "s" | "sec" | "secs" => 1,
        "m" | "min" | "mins" => 60,
        "h" | "hr" | "hrs" => 3600,
        "d" | "day" | "days" => 86400,
        _ => {
            return Err(TdError::Other(format!(
                "未知时间单位「{unit}」（用 s / m / h / d）"
            )))
        }
    };
    Ok(now_utc - n * mult)
}

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

fn cmd_clean(store: &Store, p: &Parsed) -> TdResult<()> {
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

fn cmd_apply(store: &Store, p: &Parsed) -> TdResult<()> {
    let opts = ApplyOptions {
        dry_run: p.has("dry-run"),
        verify: if p.has("sha256") {
            VerifyMode::Sha256
        } else {
            VerifyMode::CountAndSize
        },
        skip_lock_check: p.has("skip-lock-check"),
        allow_merge: p.has("merge"),
    };

    let ids = collect_target_ids(store, p, "apply")?;
    let mut noop = |_p: td_core::fsops::mover::Progress| {};

    let mut ok = 0usize;
    let mut failed = 0usize;
    let multiple = ids.len() > 1;
    for id in ids {
        let rule = match store.get_rule(id) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("  [失败] #{id}：{e}");
                failed += 1;
                continue;
            }
        };
        if multiple {
            println!("── #{id} {} ──", rule.name);
        }
        match apply_rule(store, &rule, &opts, &mut noop) {
            Ok(o) => {
                print_outcome(&o);
                ok += 1;
            }
            Err(e) => {
                println!("  [失败] #{id} {:<20} {e}", rule.name);
                failed += 1;
            }
        }
    }
    println!();
    println!(
        "{}：成功 {} 条，失败 {} 条",
        if opts.dry_run { "预演结束" } else { "执行结束" },
        ok,
        failed
    );
    if failed > 0 {
        return Err(TdError::Other(format!("有 {failed} 条未成功")));
    }
    Ok(())
}

fn cmd_restore(store: &Store, p: &Parsed) -> TdResult<()> {
    let opts = ApplyOptions {
        dry_run: p.has("dry-run"),
        verify: if p.has("sha256") {
            VerifyMode::Sha256
        } else {
            VerifyMode::CountAndSize
        },
        skip_lock_check: true,
        allow_merge: true,
    };
    let ids = collect_target_ids(store, p, "restore")?;
    let mut noop = |_p: td_core::fsops::mover::Progress| {};
    let mut ok = 0usize;
    let mut failed = 0usize;
    for id in ids {
        let rule = match store.get_rule(id) {
            Ok(r) => r,
            Err(e) => {
                println!("  [失败] #{id}：{e}");
                failed += 1;
                continue;
            }
        };
        match restore_rule(store, &rule, &opts, &mut noop) {
            Ok(o) => {
                print_outcome(&o);
                ok += 1;
            }
            Err(e) => {
                println!("  [失败] #{id} {:<20} {e}", rule.name);
                failed += 1;
            }
        }
    }
    println!();
    println!(
        "{}：成功 {} 条，失败 {} 条",
        if opts.dry_run { "预演结束" } else { "执行结束" },
        ok,
        failed
    );
    if failed > 0 {
        return Err(TdError::Other(format!("有 {failed} 条未成功")));
    }
    Ok(())
}

fn cmd_relink(store: &Store, p: &Parsed) -> TdResult<()> {
    let id = p
        .required(0, "<规则id>")?
        .parse::<i64>()
        .map_err(|_| TdError::Other("规则 id 必须是整数".into()))?;
    let rule = store.get_rule(id)?;
    let o = engine::relink_rule(store, &rule)?;
    print_outcome(&o);
    Ok(())
}

/// `--all` 时取所有规则，否则取显式列出的 id。`apply` 只处理"期望开启"的规则。
fn collect_target_ids(store: &Store, p: &Parsed, kind: &str) -> TdResult<Vec<i64>> {
    if p.has("all") {
        let rules = store.list_rules()?;
        let ids: Vec<i64> = rules
            .iter()
            .filter(|r| kind == "restore" || r.want_enabled || p.has("include-disabled"))
            .map(|r| r.id)
            .collect();
        if ids.is_empty() {
            println!("没有需要处理的规则");
        }
        return Ok(ids);
    }
    let ids = p.ids(0)?;
    if ids.is_empty() {
        return Err(TdError::Other(format!(
            "请指定规则 id，或用 `{kind} --all` 处理全部"
        )));
    }
    Ok(ids)
}

fn cmd_boot_rebuild(store: &Store, p: &Parsed) -> TdResult<()> {
    println!("开始开机重建（模拟）…");
    if p.json() {
        let report = run_boot_rebuild(store)?;
        println!("{}", serde_json::to_string_pretty(&report)?);
        return Ok(());
    }
    let report = run_boot_rebuild(store)?;
    for r in &report.roots {
        println!(
            "{} 目标根 #{} {}  [{}] {}{}",
            if r.ready { "✅" } else { "❌" },
            r.root_id,
            r.path,
            r.label,
            if r.ready {
                format!("等待 {}ms", r.waited_ms)
            } else {
                "未就绪".into()
            },
            r.error
                .as_ref()
                .map(|e| format!("\n      {e}"))
                .unwrap_or_default()
        );
    }
    println!();
    println!("{}", report.one_line());
    for w in &report.warnings {
        println!("  ⚠ {w}");
    }
    for (id, e) in &report.failed {
        println!("  ❌ #{id} {e}");
    }
    if report.has_problem() {
        return Err(TdError::Other("开机重建存在未就绪项".into()));
    }
    Ok(())
}

fn cmd_export(store: &Store, p: &Parsed) -> TdResult<()> {
    let path = p.required(0, "<输出文件.json>")?;
    let bundle = portable::export_bundle(store)?;
    let json = serde_json::to_string_pretty(&bundle)?;
    std::fs::write(&path, json.as_bytes())
        .map_err(|e| TdError::Other(format!("写入 {path} 失败：{e}")))?;
    println!(
        "已导出 {} 个目标根、{} 条规则 → {path}",
        bundle.roots.len(),
        bundle.rules.len()
    );
    println!("提示：跨机器分享时可把目标根路径改成 {{RAM_ROOT}}，导入时用 --ram-root 指定");
    Ok(())
}

fn cmd_import(store: &Store, p: &Parsed) -> TdResult<()> {
    let path = p.required(0, "<输入文件.json>")?;
    let text = std::fs::read_to_string(&path)
        .map_err(|e| TdError::Other(format!("读取 {path} 失败：{e}")))?;
    let bundle: portable::Bundle = serde_json::from_str(&text)
        .map_err(|e| TdError::Other(format!("解析 {path} 失败：{e}")))?;

    if bundle.app != portable::APP_TAG {
        println!(
            "警告：这份配置来自 {}，不是 TempDirector 导出的，将尽力导入",
            bundle.app
        );
    }
    let report = portable::import_bundle(store, &bundle, p.value("ram-root"), p.has("dry-run"))?;
    println!("{}", report.one_line());
    for e in &report.errors {
        println!("  ⚠ {e}");
    }
    Ok(())
}

fn cmd_events(store: &Store, p: &Parsed) -> TdResult<()> {
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

fn cmd_ops(store: &Store, p: &Parsed) -> TdResult<()> {
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

fn cmd_volume(p: &Parsed) -> TdResult<()> {
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

// ---------------------------------------------------------------------------
// 提权判定
// ---------------------------------------------------------------------------

/// 当前进程是否已提权。
fn is_elevated() -> bool {
    td_core::elevate::is_elevated()
}

fn cmd_autostart(p: &Parsed) -> TdResult<()> {
    use td_core::autostart;
    match p.pos(0).unwrap_or("status") {
        "status" => {
            let st = autostart::status();
            println!(
                "{} 任务名：{}",
                if st.registered { "✅ 已注册" } else { "❌ 未注册" },
                st.task_name
            );
            println!("   {}", st.message);
            if st.registered {
                println!("   触发：用户登录时（InteractiveToken / 最低权限）");
                println!("   说明：任务里只跑 `boot-rebuild`，失败结果会落库并在界面顶部提示");
            }
            Ok(())
        }
        "enable" => {
            let exe = match p.value("exe") {
                Some(v) => PathBuf::from(v),
                None => autostart::preferred_exe(),
            };
            let cmd = autostart::enable(&exe, &autostart::extra_args_from_env())?;
            println!("✅ 已注册开机自启");
            println!("   任务名：{}", autostart::TASK_NAME);
            println!("   命令：{cmd}");
            println!("   立即验证一次：tempdirector autostart run");
            Ok(())
        }
        "disable" => {
            println!("{}", autostart::disable()?);
            Ok(())
        }
        "run" => {
            println!("{}", autostart::run_now()?);
            println!("（任务在后台跑，几秒后可用 `tempdirector ops` 或界面「运维日志」查看结果）");
            Ok(())
        }
        other => Err(TdError::Other(format!(
            "未知的 autostart 子命令：{other}（可用：status / enable / disable / run）"
        ))),
    }
}

// ---------------------------------------------------------------------------
// main
// ---------------------------------------------------------------------------

fn run() -> TdResult<()> {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    if argv.is_empty() {
        print_help();
        return Ok(());
    }

    // `--data-dir` 允许放在任何位置：先扫一遍再决定数据根。
    let mut i = 0;
    while i < argv.len() {
        if argv[i] == "--data-dir" {
            if let Some(v) = argv.get(i + 1) {
                std::env::set_var(paths::DATA_DIR_ENV, v);
            }
        } else if let Some(v) = argv[i].strip_prefix("--data-dir=") {
            std::env::set_var(paths::DATA_DIR_ENV, v);
        }
        i += 1;
    }

    let cmd = argv[0].as_str();
    if cmd == "help" || cmd == "-h" || cmd == "--help" {
        print_help();
        return Ok(());
    }
    if cmd == "version" || cmd == "--version" || cmd == "-V" {
        return cmd_version();
    }

    // 提权 worker 模式：必须在打开数据库之前处理 —— 退出码要精确
    // （0 成功 / 2 部分失败 / 1 致命），而且它自己开库、自己写结果文件。
    if cmd == "elevated" {
        let job = argv
            .get(1)
            .ok_or_else(|| TdError::Other("用法：tempdirector elevated <job.json>".into()))?;
        let code = td_core::worker::run_job_to_exit_code(std::path::Path::new(job));
        std::process::exit(code);
    }

    paths::ensure_dirs().map_err(|e| TdError::Other(format!("创建数据目录失败：{e}")))?;
    let store = Store::open_default()?;

    let rest: Vec<String> = argv[1..].to_vec();
    let p = Parsed::parse(&rest);

    match cmd {
        "doctor" => cmd_doctor(&store),
        "roots" => cmd_roots(&store, &p),
        "rules" => cmd_rules(&store, &p),
        "status" => cmd_status(&store, &p),
        "locks" => cmd_locks(&store, &p),
        "clean" => cmd_clean(&store, &p),
        "apply" => cmd_apply(&store, &p),
        "restore" => cmd_restore(&store, &p),
        "relink" => cmd_relink(&store, &p),
        "boot-rebuild" => cmd_boot_rebuild(&store, &p),
        "volume" => cmd_volume(&p),
        "autostart" => cmd_autostart(&p),
        "export" => cmd_export(&store, &p),
        "import" => cmd_import(&store, &p),
        "events" => cmd_events(&store, &p),
        "ops" => cmd_ops(&store, &p),
        other => {
            print_help();
            Err(TdError::Other(format!("未知命令：{other}")))
        }
    }
}

fn print_help() {
    println!(
        r#"TempDirector —— 把软件缓存目录重定向到 RAM 盘的软链接管理器

用法：tempdirector <命令> [参数] [--json]

全局参数：
  --data-dir <路径>          覆盖数据目录（等效环境变量 TEMPDIRECTOR_DATA_DIR）

=== 概览 ===
  doctor                     环境自检：数据目录、链接能力、目标盘、规则状态、待修复项
  version                    版本信息

=== 目标根（RAM 盘）===
  roots list
  roots add <名称> <路径> [--timeout 毫秒] [--hdd]
  roots rm <id>
  volume <R: 或 R:\Dir> [--timeout 毫秒]     探测 / 等待卷就绪

=== 规则 ===
  rules list [--enabled] [--category 分类] [--search 关键词] [--json]
  rules add <名称> <源路径模板> <目标子目录> [--root id] [--category 分类]
            [--link-type junction|symlink_dir] [--enable] [--no-boot] [--notes 备注]
  rules rm <id...>
  rules enable|disable <id...|--all>

=== 体检与操作 ===
  status [--enabled|--all] [--category 分类] [--search 关键词] [--no-persist] [--json]
  locks <规则id|目录路径> [--json]          检测占用并指出是哪个进程占着
  clean <规则id|目录路径> <策略> [--yes] [--json]
        清理文件夹（永久删除，默认只预演，加 --yes 才执行）
        策略四选一：
          --empty                        清空全部文件
          --yesterday                    昨天及更早修改的（本地零点前）
          --older-than <时长>            早于某时刻，如 1h / 30m / 7d / today / Unix秒
          --from <时> --to <时>          指定时间段 [from,to)，如 --from 7d --to 1d
  apply <id...|--all> [--dry-run] [--merge] [--sha256] [--skip-lock-check]
  restore <id...|--all> [--dry-run] [--sha256]
  relink <id>                                只重建链接（不动数据）

=== 开机重建 ===
  boot-rebuild [--json]
  autostart status                           查看"登录时自动重建"任务是否已注册
  autostart enable [--exe <路径>]            注册（用户级，不需要管理员权限）
  autostart disable
  autostart run                              立刻手动触发一次，验证配置

=== 提权 worker（一般不需要手动调）===
  elevated <job.json>                        以管理员身份执行一份 job（只认规则 id，不认路径）

=== 配置迁移 ===
  export <文件.json>
  import <文件.json> [--dry-run] [--ram-root R:]

=== 审计 ===
  events [--limit N]
  ops [--limit N]                            操作流水（含待修复项）

分类取值：browser / design / dev / system / media / office / game / other

示例：
  tempdirector roots add RAMDisk R:
  tempdirector rules add "Chrome 缓存" "%LOCALAPPDATA%\Google\Chrome\User Data\Default\Cache" browsers\chrome-cache --category browser --enable
  tempdirector status --all
  tempdirector apply --all --dry-run
  tempdirector apply 1
  tempdirector restore 1
  tempdirector clean 3 --older-than 1d          预演：规则#3 真实目录里 1 天前的文件
  tempdirector clean 3 --older-than 1d --yes    确认后执行删除
  tempdirector autostart enable
"#
    );
}

fn main() -> ExitCode {
    // 让中文在 Windows 控制台正常显示
    #[cfg(windows)]
    unsafe {
        windows_sys::Win32::System::Console::SetConsoleOutputCP(65001);
    }

    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!();
            eprintln!("❌ {e}");
            ExitCode::FAILURE
        }
    }
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
    fn parse_when_relative_and_keywords() {
        assert_eq!(parse_when("1h", NOW, TOD).unwrap(), NOW - 3600);
        assert_eq!(parse_when("30m", NOW, TOD).unwrap(), NOW - 1800);
        assert_eq!(parse_when("7d", NOW, TOD).unwrap(), NOW - 7 * 86400);
        assert_eq!(parse_when("90s", NOW, TOD).unwrap(), NOW - 90);
        assert_eq!(parse_when("today", NOW, TOD).unwrap(), NOW - TOD);
        assert_eq!(parse_when("yesterday", NOW, TOD).unwrap(), NOW - TOD - 86400);
        assert_eq!(parse_when("1234567890", NOW, TOD).unwrap(), 1234567890);
        assert!(parse_when("abc", NOW, TOD).is_err());
        assert!(parse_when("5x", NOW, TOD).is_err(), "未知单位应报错");
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

    #[test]
    fn ago_text_is_relative() {
        assert_eq!(ago_text(0), "现在");
        assert!(ago_text(3600).ends_with('前'));
        assert!(ago_text(-5).ends_with("在"), "未来时刻归一为「现在」");
    }

    #[test]
    fn human_duration_uses_big_units() {
        assert_eq!(human_duration(45), "45 秒");
        assert_eq!(human_duration(120), "2 分");
        assert_eq!(human_duration(3600), "1 小时");
        assert_eq!(human_duration(86400), "1 天");
        assert_eq!(human_duration(259200), "3 天");
        assert_eq!(human_duration(90000), "1 天 1 小时");
    }
}
