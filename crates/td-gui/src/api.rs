//! GUI 的业务层。
//!
//! **刻意不依赖任何 Tauri 类型** —— 所有函数只吃 `&Store`、吐 serde 可序列化的结构体。
//! 这样这一层可以脱离窗口独立做单元测试（`#[tauri::command]` 只是薄薄一层转发，
//! 见 `commands.rs`），也便于以后把同一套逻辑接到服务端或别的 UI 上。

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use td_core::boot::{probe_volume, run_boot_rebuild, BootReport};
use td_core::engine::{self, ApplyOptions, RuleStatus};
use td_core::error::{TdError, TdResult};
use td_core::fsops::mover::Progress;
use td_core::fsops::occupancy::{self, LockReport, LockScanOptions};
use td_core::fsops::verify::VerifyMode;
use td_core::link::detect::{detect_link, LinkState, TargetState};
use td_core::paths;
use td_core::portable::{self, ImportReport};
use td_core::store::{EventRow, NewRule, Operation, Store, CATEGORIES};
use td_core::util;

// ---------------------------------------------------------------------------
// 视图结构
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub struct RootView {
    pub id: i64,
    pub label: String,
    pub path: String,
    pub is_ramdisk: bool,
    pub wait_timeout_ms: i64,
    /// `ready` / `missing` / `not_dir` / `readonly` / `no_space_info`
    pub state: &'static str,
    pub message: String,
    pub unknown_vars: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct RuleView {
    pub id: i64,
    pub name: String,
    pub category: String,
    /// 未展开的模板（编辑表单用）。
    pub source_template: String,
    /// 展开后的真实源路径（展示用）。
    pub source: String,
    pub target: String,
    pub target_root_id: i64,
    pub target_subpath: String,
    pub link_type: String,
    pub want_enabled: bool,
    pub rebuild_on_boot: bool,
    /// 是否已存有 DACL 模板。
    pub has_acl: bool,
    pub needs_elevation: bool,
    pub notes: Option<String>,

    pub badge: String,
    pub label: String,
    pub needs_attention: bool,
    pub warnings: Vec<String>,
    pub unknown_vars: Vec<String>,
    pub last_files: Option<i64>,
    pub last_bytes: Option<i64>,
    pub applied_at: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Stats {
    pub total: usize,
    pub enabled: usize,
    pub ok: usize,
    pub warn: usize,
    pub plain: usize,
    pub other: usize,
    pub needs_attention: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct UiSnapshot {
    pub version: String,
    pub schema_version: i32,
    pub data_dir: String,
    pub db_path: String,
    pub categories: Vec<String>,
    pub roots: Vec<RootView>,
    pub rules: Vec<RuleView>,
    pub stats: Stats,
    /// 顶部横幅（就绪状态 / 待处理项 / 上次中断的操作）。
    pub banners: Vec<Banner>,
    pub open_ops: Vec<Operation>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Banner {
    /// `info` / `warn` / `error`
    pub level: &'static str,
    pub text: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ItemOutcome {
    pub rule_id: i64,
    pub name: String,
    pub ok: bool,
    pub action: String,
    pub message: String,
    pub elapsed_ms: u128,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RuleForm {
    pub name: String,
    pub category: String,
    pub source_template: String,
    pub target_root_id: i64,
    pub target_subpath: String,
    pub link_type: String,
    #[serde(default)]
    pub want_enabled: bool,
    #[serde(default = "yes")]
    pub rebuild_on_boot: bool,
    #[serde(default)]
    pub notes: Option<String>,
}

fn yes() -> bool {
    true
}

impl RuleForm {
    fn to_new_rule(&self) -> NewRule {
        NewRule {
            name: self.name.clone(),
            category: self.category.clone(),
            source_template: self.source_template.clone(),
            target_root_id: self.target_root_id,
            target_subpath: self.target_subpath.clone(),
            link_type: self.link_type.clone(),
            want_enabled: self.want_enabled,
            rebuild_on_boot: self.rebuild_on_boot,
            notes: self.notes.clone(),
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct ApplyOpts {
    #[serde(default)]
    pub dry_run: bool,
    /// 目标目录非空时允许合并（同路径取较新者）。
    #[serde(default)]
    pub allow_merge: bool,
    /// 跳过占用检测（危险）。
    #[serde(default)]
    pub skip_lock_check: bool,
    /// 逐文件 SHA-256（慢）。
    #[serde(default)]
    pub sha256: bool,
}

impl ApplyOpts {
    fn to_engine(&self) -> ApplyOptions {
        ApplyOptions {
            dry_run: self.dry_run,
            verify: if self.sha256 {
                VerifyMode::Sha256
            } else {
                VerifyMode::CountAndSize
            },
            skip_lock_check: self.skip_lock_check,
            allow_merge: self.allow_merge,
        }
    }
}

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

// ---------------------------------------------------------------------------
// 读
// ---------------------------------------------------------------------------

pub fn snapshot(store: &Store) -> TdResult<UiSnapshot> {
    let roots = root_views(store)?;
    let rules = rule_views(store)?;

    let mut stats = Stats {
        total: rules.len(),
        ..Default::default()
    };
    for r in &rules {
        if r.want_enabled {
            stats.enabled += 1;
        }
        if r.needs_attention {
            stats.needs_attention += 1;
        }
        match r.badge.as_str() {
            "✅" => stats.ok += 1,
            "⚠️" => stats.warn += 1,
            "❌" => stats.plain += 1,
            _ => stats.other += 1,
        }
    }

    let open_ops = store.list_open_ops()?;
    let mut banners: Vec<Banner> = Vec::new();

    // 登录任务是静默跑的：它出问题必须在这里被看到，而不是躺在日志里
    if let Some((at, summary)) = last_boot_problem(store) {
        banners.push(Banner {
            level: "warn",
            text: format!("上次开机重建（{at}）有需要留意的地方：{summary}"),
        });
    }

    let roots_ready = roots.iter().filter(|r| r.state == "ready").count();
    if roots.is_empty() {
        banners.push(Banner {
            level: "warn",
            text: "还没有配置目标根（RAM 盘），先在「设置」里添加一个，例如 R:".into(),
        });
    } else if roots_ready == 0 {
        banners.push(Banner {
            level: "error",
            text: "所有目标根都不可用 —— RAM 盘可能没有创建或没有挂载，此时无法建立链接".into(),
        });
    }
    if stats.needs_attention > 0 {
        banners.push(Banner {
            level: "warn",
            text: format!("有 {} 条规则需要处理（断链 / 真实目录 / 外来重解析点）", stats.needs_attention),
        });
    }
    if !open_ops.is_empty() {
        banners.push(Banner {
            level: "error",
            text: format!(
                "有 {} 条上次中断的操作未收尾，请查看「运维」面板确认后处理",
                open_ops.len()
            ),
        });
    }
    for r in roots.iter().filter(|r| r.state != "ready") {
        banners.push(Banner {
            level: "warn",
            text: format!("目标根 #{} {} 异常：{}", r.id, r.path, r.message),
        });
    }

    Ok(UiSnapshot {
        version: VERSION.to_string(),
        schema_version: store.user_version()?,
        data_dir: paths::data_dir().display().to_string(),
        db_path: paths::db_path().display().to_string(),
        categories: CATEGORIES.iter().map(|s| s.to_string()).collect(),
        roots,
        rules,
        stats,
        banners,
        open_ops,
    })
}

pub fn root_views(store: &Store) -> TdResult<Vec<RootView>> {
    let mut out = Vec::new();
    for r in store.list_roots()? {
        let (path, unknown) = r.resolved_path();
        let (state, message) = probe_volume(&path);
        out.push(RootView {
            id: r.id,
            label: r.label,
            path: r.path,
            is_ramdisk: r.is_ramdisk,
            wait_timeout_ms: r.wait_timeout_ms,
            state,
            message,
            unknown_vars: unknown,
        });
    }
    Ok(out)
}

pub fn rule_views(store: &Store) -> TdResult<Vec<RuleView>> {
    let mut out = Vec::new();
    for rule in store.list_rules()? {
        let resolved = engine::resolve(store, &rule)?;
        let status = engine::rule_status(store, &rule)?;
        out.push(merge_view(rule, resolved, status));
    }
    Ok(out)
}

fn merge_view(
    rule: td_core::store::Rule,
    resolved: engine::Resolved,
    status: RuleStatus,
) -> RuleView {
    RuleView {
        id: rule.id,
        name: rule.name,
        category: rule.category,
        source_template: rule.source_template,
        source: resolved.source.display().to_string(),
        target: resolved.target.display().to_string(),
        target_root_id: rule.target_root_id,
        target_subpath: rule.target_subpath,
        link_type: rule.link_type,
        want_enabled: rule.want_enabled,
        rebuild_on_boot: rule.rebuild_on_boot,
        has_acl: rule.acl_sddl.is_some(),
        needs_elevation: rule.needs_elevation,
        notes: rule.notes,
        badge: status.badge.to_string(),
        label: status.label,
        needs_attention: status.needs_attention,
        warnings: status.warnings,
        unknown_vars: status.unknown_vars,
        last_files: rule.last_files,
        last_bytes: rule.last_bytes,
        applied_at: rule.applied_at,
    }
}

pub fn recent_events(store: &Store, limit: i64) -> TdResult<Vec<EventRow>> {
    store.recent_events(limit)
}

pub fn recent_ops(store: &Store, limit: i64) -> TdResult<Vec<Operation>> {
    store.recent_ops(limit)
}

// ---------------------------------------------------------------------------
// 目标根
// ---------------------------------------------------------------------------

pub fn add_root(
    store: &Store,
    label: &str,
    path: &str,
    is_ramdisk: bool,
    wait_timeout_ms: i64,
) -> TdResult<i64> {
    if label.trim().is_empty() {
        return Err(TdError::InvalidPath("名称不能为空".into()));
    }
    if path.trim().is_empty() {
        return Err(TdError::InvalidPath("路径不能为空，例如 R:".into()));
    }
    store.ensure_root(label, path, is_ramdisk, wait_timeout_ms)
}

pub fn delete_root(store: &Store, id: i64) -> TdResult<()> {
    store.delete_root(id)
}

// ---------------------------------------------------------------------------
// 规则
// ---------------------------------------------------------------------------

pub fn add_rule(store: &Store, form: &RuleForm) -> TdResult<i64> {
    validate_form(form)?;
    store.insert_rule(&form.to_new_rule())
}

pub fn update_rule(store: &Store, id: i64, form: &RuleForm) -> TdResult<()> {
    validate_form(form)?;
    store.update_rule(id, &form.to_new_rule())
}

fn validate_form(form: &RuleForm) -> TdResult<()> {
    if form.name.trim().is_empty() {
        return Err(TdError::InvalidPath("规则名称不能为空".into()));
    }
    if form.source_template.trim().is_empty() {
        return Err(TdError::InvalidPath("源路径不能为空".into()));
    }
    if CATEGORIES.contains(&form.category.as_str()) == false {
        return Err(TdError::InvalidPath(format!(
            "未知分类 {}，可用：{}",
            form.category,
            CATEGORIES.join(" / ")
        )));
    }
    if form.target_subpath.trim().is_empty() {
        return Err(TdError::InvalidPath(
            "目标子目录不能为空（它是 RAM 盘下的相对路径，例如 browsers\\chrome）".into(),
        ));
    }
    if td_core::link::LinkType::parse(&form.link_type).is_none() {
        return Err(TdError::InvalidPath(
            "链接类型只能是 junction 或 symlink_dir".into(),
        ));
    }
    Ok(())
}

pub fn delete_rules(store: &Store, ids: &[i64]) -> TdResult<usize> {
    let mut n = 0;
    for id in ids {
        store.delete_rule(*id)?;
        n += 1;
    }
    Ok(n)
}

pub fn set_enabled(store: &Store, ids: &[i64], enabled: bool) -> TdResult<usize> {
    for id in ids {
        store.set_rule_enabled(*id, enabled)?;
    }
    Ok(ids.len())
}

/// 保存一条目录的 DACL 作为重建模板（用于需要提权的规则）。
pub fn capture_acl(store: &Store, id: i64) -> TdResult<String> {
    let rule = store.get_rule(id)?;
    let resolved = engine::resolve(store, &rule)?;
    if !resolved.source.is_dir() {
        return Err(TdError::InvalidPath(format!(
            "源目录不存在，无法导出权限：{}",
            resolved.source.display()
        )));
    }
    let sddl = td_core::boot::acl::export_sddl(&resolved.source)?;
    store.set_rule_flags(id, rule.rebuild_on_boot, &rule.link_type, Some(&sddl))?;
    store.log_event(
        Some(id),
        "info",
        "acl_captured",
        &format!("{}：已保存目录权限模板", rule.name),
    )?;
    Ok(sddl)
}

pub fn clear_acl(store: &Store, id: i64) -> TdResult<()> {
    let rule = store.get_rule(id)?;
    store.set_rule_flags(id, rule.rebuild_on_boot, &rule.link_type, None)
}

// ---------------------------------------------------------------------------
// 应用 / 恢复
// ---------------------------------------------------------------------------

/// 批量应用。**单条失败不影响其它条目**，逐条收集结果。
/// `on_progress` 会在每条开始前与过程中的关键节点被调用。
pub fn apply_rules(
    store: &Store,
    ids: &[i64],
    opts: &ApplyOpts,
    on_progress: &mut dyn FnMut(usize, usize, &str, &Progress),
) -> Vec<ItemOutcome> {
    let total = ids.len();
    let mut out = Vec::with_capacity(total);
    for (i, id) in ids.iter().enumerate() {
        let rule = match store.get_rule(*id) {
            Ok(r) => r,
            Err(e) => {
                out.push(ItemOutcome {
                    rule_id: *id,
                    name: format!("#{id}"),
                    ok: false,
                    action: "not_found".into(),
                    message: e.to_string(),
                    elapsed_ms: 0,
                });
                continue;
            }
        };
        on_progress(i, total, &rule.name, &Progress::Enumerated { files: 0, bytes: 0 });
        let mut sink = |p: Progress| on_progress(i, total, &rule.name, &p);
        match engine::apply_rule(store, &rule, &opts.to_engine(), &mut sink) {
            Ok(o) => out.push(ItemOutcome {
                rule_id: o.rule_id,
                name: o.name,
                ok: true,
                action: o.action.to_string(),
                message: o.message,
                elapsed_ms: o.elapsed_ms,
            }),
            Err(e) => out.push(ItemOutcome {
                rule_id: rule.id,
                name: rule.name,
                ok: false,
                action: "failed".into(),
                message: e.to_string(),
                elapsed_ms: 0,
            }),
        }
    }
    out
}

pub fn restore_rules(
    store: &Store,
    ids: &[i64],
    opts: &ApplyOpts,
    on_progress: &mut dyn FnMut(usize, usize, &str, &Progress),
) -> Vec<ItemOutcome> {
    let total = ids.len();
    let mut out = Vec::with_capacity(total);
    for (i, id) in ids.iter().enumerate() {
        let rule = match store.get_rule(*id) {
            Ok(r) => r,
            Err(e) => {
                out.push(ItemOutcome {
                    rule_id: *id,
                    name: format!("#{id}"),
                    ok: false,
                    action: "not_found".into(),
                    message: e.to_string(),
                    elapsed_ms: 0,
                });
                continue;
            }
        };
        on_progress(i, total, &rule.name, &Progress::Deleting { files: 0 });
        let mut sink = |p: Progress| on_progress(i, total, &rule.name, &p);
        match engine::restore_rule(store, &rule, &opts.to_engine(), &mut sink) {
            Ok(o) => out.push(ItemOutcome {
                rule_id: o.rule_id,
                name: o.name,
                ok: true,
                action: o.action.to_string(),
                message: o.message,
                elapsed_ms: o.elapsed_ms,
            }),
            Err(e) => out.push(ItemOutcome {
                rule_id: rule.id,
                name: rule.name,
                ok: false,
                action: "failed".into(),
                message: e.to_string(),
                elapsed_ms: 0,
            }),
        }
    }
    out
}

pub fn relink(store: &Store, id: i64) -> TdResult<ItemOutcome> {
    let rule = store.get_rule(id)?;
    let o = engine::relink_rule(store, &rule)?;
    Ok(ItemOutcome {
        rule_id: o.rule_id,
        name: o.name,
        ok: true,
        action: o.action.to_string(),
        message: o.message,
        elapsed_ms: o.elapsed_ms,
    })
}

pub fn check_locks(store: &Store, id: i64) -> TdResult<LockReport> {
    let rule = store.get_rule(id)?;
    let resolved = engine::resolve(store, &rule)?;
    if !resolved.source.is_dir() {
        return Err(TdError::InvalidPath(format!(
            "源目录不存在或不是目录，无法检测占用：{}",
            resolved.source.display()
        )));
    }
    let report = occupancy::scan_locks(&resolved.source, &LockScanOptions::default())?;
    if !report.is_clear() {
        store.log_event(
            Some(id),
            "warn",
            "blocked",
            &format!("{}：{}", rule.name, report.summary()),
        )?;
    }
    Ok(report)
}

// ---------------------------------------------------------------------------
// 开机重建 / 迁移
// ---------------------------------------------------------------------------

pub fn boot_rebuild(store: &Store) -> TdResult<BootReport> {
    run_boot_rebuild(store)
}

pub fn export_config(store: &Store, path: &Path) -> TdResult<(usize, usize)> {
    let bundle = portable::export_bundle(store)?;
    let json = serde_json::to_string_pretty(&bundle)?;
    std::fs::write(path, json.as_bytes())
        .map_err(|e| TdError::Other(format!("写入 {} 失败：{e}", path.display())))?;
    Ok((bundle.roots.len(), bundle.rules.len()))
}

pub fn import_config(
    store: &Store,
    path: &Path,
    ram_root: Option<&str>,
    dry_run: bool,
) -> TdResult<ImportReport> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| TdError::Other(format!("读取 {} 失败：{e}", path.display())))?;
    let bundle: portable::Bundle = serde_json::from_str(&text)
        .map_err(|e| TdError::Other(format!("解析 {} 失败：{e}", path.display())))?;
    portable::import_bundle(store, &bundle, ram_root, dry_run)
}

// ---------------------------------------------------------------------------
// 小工具（给 UI 展示用）
// ---------------------------------------------------------------------------

/// 给 UI 的状态文案（把 LinkState 摊平成一行摘要）。
pub fn describe_state(state: &LinkState) -> String {
    match state {
        LinkState::LinkOk {
            target_state: TargetState::Empty,
            ..
        } => "链接已就绪，目标为空（开机重建后的正常形态）".into(),
        LinkState::LinkOk { .. } => "链接正常".into(),
        other => other.label_zh(),
    }
}

/// 源路径的父目录（给"在资源管理器里打开"用）。
pub fn parent_of_display(path: &str) -> Option<PathBuf> {
    let p = PathBuf::from(path);
    if p.is_dir() {
        Some(p)
    } else {
        p.parent()
            .filter(|q| q.is_dir())
            .map(|q| q.to_path_buf())
    }
}

pub fn human_bytes(n: u64) -> String {
    util::human_bytes(n)
}

/// 校验源路径，返回警告与是否需要提权（表单实时提示用）。
pub fn inspect_source(template: &str) -> TdResult<SourceInspection> {
    let (expanded, unknown) = util::expand_path(template);
    let pf = td_core::safety::preflight_source(&expanded);
    Ok(SourceInspection {
        expanded: expanded.display().to_string(),
        exists: expanded.exists(),
        blocking: pf.blocking,
        warnings: pf.warnings,
        requires_elevation: pf.requires_elevation,
        unknown_vars: unknown,
    })
}

#[derive(Debug, Clone, Serialize)]
pub struct SourceInspection {
    pub expanded: String,
    pub exists: bool,
    pub blocking: Vec<String>,
    pub warnings: Vec<String>,
    pub requires_elevation: bool,
    pub unknown_vars: Vec<String>,
}

/// 目标路径是否已被占用（表单校验用）。
pub fn inspect_target(store: &Store, root_id: i64, subpath: &str) -> TdResult<TargetInspection> {
    let root = store.get_root(root_id)?;
    let (base, unknown) = root.resolved_path();
    let sub = subpath.trim().trim_start_matches(['\\', '/']);
    let target = if sub.is_empty() {
        base.clone()
    } else {
        base.join(sub)
    };
    let state = detect_link(&target, None);
    Ok(TargetInspection {
        target: target.display().to_string(),
        root_ready: probe_volume(&base).0 == "ready",
        exists: target.exists(),
        occupied: matches!(
            state,
            LinkState::PlainDir | LinkState::PlainFile | LinkState::ForeignReparse { .. }
        ),
        state: describe_state(&state),
        unknown_vars: unknown,
    })
}

#[derive(Debug, Clone, Serialize)]
pub struct TargetInspection {
    pub target: String,
    pub root_ready: bool,
    pub exists: bool,
    /// 目标位置已有真实文件/目录 —— apply 需要 `allow_merge` 才能继续。
    pub occupied: bool,
    pub state: String,
    pub unknown_vars: Vec<String>,
}

// ---------------------------------------------------------------------------
// 开机自启 / 提权（M3）
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub struct AutostartView {
    pub registered: bool,
    pub task_name: String,
    pub command: String,
    pub message: String,
    /// 当前进程是否已提权（界面据此解释"为什么这次不用弹 UAC"）。
    pub elevated: bool,
    pub data_dir: String,
    /// 便携模式下会写进任务的参数尾巴。
    pub extra_args: String,
    pub recommended_exe: String,
}

pub fn autostart_status() -> AutostartView {
    let st = td_core::autostart::status();
    AutostartView {
        registered: st.registered,
        task_name: st.task_name,
        command: st.command,
        message: st.message,
        elevated: td_core::elevate::is_elevated(),
        data_dir: paths::data_dir().display().to_string(),
        extra_args: td_core::autostart::extra_args_from_env(),
        recommended_exe: td_core::autostart::preferred_exe().display().to_string(),
    }
}

pub fn autostart_enable() -> TdResult<String> {
    let exe = td_core::autostart::preferred_exe();
    td_core::autostart::enable(&exe, &td_core::autostart::extra_args_from_env())
}

pub fn autostart_disable() -> TdResult<String> {
    td_core::autostart::disable()
}

pub fn autostart_run_now() -> TdResult<String> {
    td_core::autostart::run_now()
}

/// 上一次开机重建是不是有问题（界面顶部提示用）。返回 `(时间, 摘要)`。
pub fn last_boot_problem(store: &Store) -> Option<(String, String)> {
    let json = store.get_setting("last_boot_report").ok().flatten()?;
    let report: BootReport = serde_json::from_str(&json).ok()?;
    if !report.has_problem() {
        return None;
    }
    let at = store
        .get_setting("last_boot_at")
        .ok()
        .flatten()
        .unwrap_or_default();
    Some((at, report.one_line()))
}

/// 通过提权 worker 执行 apply / restore。
///
/// 父进程**不开新连接也不加锁**：worker 是独立进程、自己开库（WAL 下多连接没问题），
/// 干完写一份 result.json，这里读回来转成界面用的结果。
pub fn run_elevated_job(
    store: &Store,
    kind: &str,
    ids: &[i64],
    opts: &ApplyOpts,
) -> TdResult<Vec<ItemOutcome>> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    if opts.dry_run {
        return Err(TdError::Other(
            "预演不需要提权：请先直接预演（它只读不写）".into(),
        ));
    }

    let job = td_core::worker::Job {
        schema: td_core::worker::JOB_VERSION,
        kind: kind.to_string(),
        rules: ids.to_vec(),
        dry_run: false,
        allow_merge: opts.allow_merge,
        sha256: opts.sha256,
        origin_pid: std::process::id(),
    };
    let job_path = td_core::worker::write_job(&job)?;
    let exe = std::env::current_exe()
        .map_err(|e| TdError::Other(format!("取当前程序路径失败：{e}")))?;
    let args = format!(
        "elevated {}",
        td_core::elevate::quote_arg(&job_path.to_string_lossy())
    );

    let code = td_core::elevate::run_elevated_wait(&exe, &args)?;
    let result = td_core::worker::read_result(&job_path);
    // 清理：job 与结果文件都是一次性的
    let _ = std::fs::remove_file(&job_path);
    let _ = std::fs::remove_file(td_core::worker::result_path_for(&job_path));

    match result {
        Some(r) => {
            store.log_event(
                None,
                if r.ok { "info" } else { "warn" },
                "elevated_job",
                &format!("{kind}（提权）：{}", r.one_line()),
            )?;
            Ok(r
                .items
                .into_iter()
                .map(|i| ItemOutcome {
                    rule_id: i.rule_id,
                    name: i.name,
                    ok: i.ok,
                    action: i.action,
                    message: i.message,
                    elapsed_ms: i.elapsed_ms,
                })
                .collect())
        }
        None => Err(TdError::Other(format!(
            "提权进程没留下结果（退出码 {code}）。可能是它启动失败，或被安全软件拦下了。"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    struct Env {
        base: PathBuf,
        store: Store,
        source: PathBuf,
        ram: PathBuf,
    }

    fn setup(tag: &str) -> Env {
        let base = std::env::temp_dir().join(format!("td_gui_{tag}_{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        let source = base.join("cache");
        let ram = base.join("ram");
        fs::create_dir_all(&source).unwrap();
        fs::create_dir_all(&ram).unwrap();
        fs::write(source.join("a.bin"), vec![1u8; 1024]).unwrap();

        let store = Store::open_in_memory().unwrap();
        store
            .insert_root("RAM", &ram.to_string_lossy(), true, 5000)
            .unwrap();
        Env {
            base,
            store,
            source,
            ram,
        }
    }

    fn form(env: &Env, name: &str, sub: &str) -> RuleForm {
        RuleForm {
            name: name.into(),
            category: "dev".into(),
            source_template: env.source.to_string_lossy().into_owned(),
            target_root_id: 1,
            target_subpath: sub.into(),
            link_type: "junction".into(),
            want_enabled: true,
            rebuild_on_boot: true,
            notes: None,
        }
    }

    #[test]
    fn snapshot_reports_banners_and_stats() {
        let env = setup("snapshot");
        let s = snapshot(&env.store).unwrap();
        assert_eq!(s.stats.total, 0);
        assert_eq!(s.roots.len(), 1);
        assert_eq!(s.roots[0].state, "ready");
        assert!(
            !s.banners
                .iter()
                .any(|b| b.text.contains("还没有配置目标根")),
            "目标根就绪时不应出现「未配置」横幅：{:?}",
            s.banners
        );
        let _ = fs::remove_dir_all(&env.base);
    }

    #[test]
    fn add_rule_then_apply_then_restore_through_api() {
        let env = setup("roundtrip");
        let id = add_rule(&env.store, &form(&env, "测试", "dev\\x")).unwrap();

        let snap = snapshot(&env.store).unwrap();
        assert_eq!(snap.stats.total, 1);
        assert_eq!(snap.rules[0].badge, "❌", "初始应是未链接");

        let opts = ApplyOpts::default();
        let mut calls = 0usize;
        let res = apply_rules(&env.store, &[id], &opts, &mut |i, total, name, _p| {
            calls += 1;
            assert_eq!(i, 0);
            assert_eq!(total, 1);
            assert!(!name.is_empty());
        });
        assert_eq!(res.len(), 1);
        assert!(res[0].ok, "{}", res[0].message);
        assert_eq!(res[0].action, "moved_and_linked");
        assert!(calls > 0);

        let snap = snapshot(&env.store).unwrap();
        assert_eq!(snap.rules[0].badge, "✅");
        assert_eq!(snap.stats.ok, 1);

        let res = restore_rules(&env.store, &[id], &opts, &mut |_, _, _, _| {});
        assert!(res[0].ok, "{}", res[0].message);
        let snap = snapshot(&env.store).unwrap();
        assert_eq!(snap.stats.plain, 1);
        let _ = fs::remove_dir_all(&env.base);
    }

    /// 同一测试里加多条规则时，源路径必须互不相同 —— 规则库对 `source_norm`
    /// 有 UNIQUE 约束（同一个缓存目录配两条规则是错的），这是有意的。
    fn form_at(env: &Env, name: &str, src: &str, sub: &str) -> RuleForm {
        let mut f = form(env, name, sub);
        f.source_template = src.to_string();
        f
    }

    #[test]
    fn batch_apply_collects_failures_without_aborting() {
        let env = setup("batch");
        let good_src = env.base.join("cache-good");
        fs::create_dir_all(&good_src).unwrap();
        fs::write(good_src.join("f.bin"), b"x").unwrap();
        let good = add_rule(
            &env.store,
            &form_at(&env, "好的", &good_src.to_string_lossy(), "dev\\good"),
        )
        .unwrap();
        // 源在一个不存在的盘上 → 必然失败
        let bad = add_rule(
            &env.store,
            &form_at(&env, "坏的", r"Z:\Nope\Cache", "dev\\bad"),
        )
        .unwrap();

        let res = apply_rules(&env.store, &[bad, good], &ApplyOpts::default(), &mut |_, _, _, _| {});
        assert_eq!(res.len(), 2);
        assert!(!res[0].ok, "坏规则应当失败");
        assert!(res[1].ok, "好规则不受影响：{}", res[1].message);
        let _ = fs::remove_dir_all(&env.base);
    }

    #[test]
    fn acl_capture_and_clear() {
        let env = setup("acl");
        let id = add_rule(&env.store, &form(&env, "权限", "dev\\acl")).unwrap();

        let sddl = capture_acl(&env.store, id).unwrap();
        assert!(sddl.starts_with('D'), "SDDL 应以 D 开头：{sddl}");
        assert!(snapshot(&env.store).unwrap().rules[0].has_acl);

        clear_acl(&env.store, id).unwrap();
        assert!(!snapshot(&env.store).unwrap().rules[0].has_acl);
        let _ = fs::remove_dir_all(&env.base);
    }

    #[test]
    fn form_validation_rejects_bad_input() {
        let env = setup("validate");
        let mut f = form(&env, "x", "dev\\x");
        f.name = "  ".into();
        assert!(add_rule(&env.store, &f).is_err());

        let mut f = form(&env, "x", "dev\\x");
        f.category = "nonsense".into();
        assert!(add_rule(&env.store, &f).is_err());

        let mut f = form(&env, "x", "  ");
        f.target_subpath = "   ".into();
        assert!(add_rule(&env.store, &f).is_err());

        let mut f = form(&env, "x", "dev\\x");
        f.link_type = "hardlink".into();
        assert!(add_rule(&env.store, &f).is_err());
        let _ = fs::remove_dir_all(&env.base);
    }

    #[test]
    fn export_import_roundtrip_through_api() {
        let env = setup("portable");
        add_rule(&env.store, &form(&env, "导出测试", "dev\\p")).unwrap();
        let file = env.base.join("cfg.json");

        let (roots, rules) = export_config(&env.store, &file).unwrap();
        assert_eq!((roots, rules), (1, 1));

        let other = Store::open_in_memory().unwrap();
        let rep = import_config(&other, &file, None, false).unwrap();
        assert_eq!(rep.rules_created, 1);
        assert_eq!(rule_views(&other).unwrap()[0].name, "导出测试");
        let _ = fs::remove_dir_all(&env.base);
    }

    #[test]
    fn inspect_source_flags_system_dirs_and_elevation() {
        let ins = inspect_source(r"C:\Windows\System32").unwrap();
        assert!(!ins.blocking.is_empty());

        let ins = inspect_source(r"C:\Program Files\Foo\Cache").unwrap();
        assert!(ins.requires_elevation);
        assert!(ins.blocking.is_empty());

        let ins = inspect_source(r"%NO_SUCH_VAR_XYZ%\cache").unwrap();
        assert_eq!(ins.unknown_vars.len(), 1);
    }

    #[test]
    fn inspect_target_detects_occupancy() {
        let env = setup("target");
        let occupied = env.ram.join("dev").join("busy");
        fs::create_dir_all(&occupied).unwrap();
        fs::write(occupied.join("x"), b"1").unwrap();

        let t = inspect_target(&env.store, 1, "dev\\busy").unwrap();
        assert!(t.root_ready);
        assert!(t.occupied, "已有内容的目录应报占用");

        let t = inspect_target(&env.store, 1, "dev\\free").unwrap();
        assert!(!t.occupied);
        assert!(!t.exists);
        let _ = fs::remove_dir_all(&env.base);
    }

    #[test]
    fn check_locks_on_clean_dir_is_clear() {
        let env = setup("locks");
        let id = add_rule(&env.store, &form(&env, "占用", "dev\\l")).unwrap();
        let report = check_locks(&env.store, id).unwrap();
        assert!(report.is_clear(), "{}", report.summary());
        assert_eq!(report.scanned, 1);
        let _ = fs::remove_dir_all(&env.base);
    }

    #[test]
    fn boot_rebuild_through_api_recovers_from_ram_loss() {
        let env = setup("boot");
        let id = add_rule(&env.store, &form(&env, "开机", "dev\\b")).unwrap();
        apply_rules(&env.store, &[id], &ApplyOpts::default(), &mut |_, _, _, _| {});
        // 模拟 RAM 盘重启
        fs::remove_dir_all(env.ram.join("dev")).unwrap();

        let report = boot_rebuild(&env.store).unwrap();
        assert_eq!(report.dirs_created, 1);
        assert_eq!(report.links_recovered, 1);
        assert!(!report.has_problem(), "{}", report.one_line());

        let snap = snapshot(&env.store).unwrap();
        assert_eq!(snap.rules[0].badge, "⚠️", "目标为空 → 警告徽标");
        assert_eq!(snap.stats.warn, 1);
        let _ = fs::remove_dir_all(&env.base);
    }

    #[test]
    fn delete_rules_and_set_enabled() {
        let env = setup("enabled");
        let a = add_rule(
            &env.store,
            &form_at(&env, "A", &env.base.join("srcA").to_string_lossy(), "dev\\a"),
        )
        .unwrap();
        let b = add_rule(
            &env.store,
            &form_at(&env, "B", &env.base.join("srcB").to_string_lossy(), "dev\\b"),
        )
        .unwrap();
        assert_eq!(set_enabled(&env.store, &[b], false).unwrap(), 1);
        assert_eq!(snapshot(&env.store).unwrap().stats.enabled, 1);
        assert_eq!(delete_rules(&env.store, &[a]).unwrap(), 1);
        assert_eq!(snapshot(&env.store).unwrap().stats.total, 1);
        let _ = fs::remove_dir_all(&env.base);
    }

    #[test]
    fn duplicate_source_path_is_rejected() {
        let env = setup("dup");
        add_rule(&env.store, &form(&env, "第一条", "dev\\one")).unwrap();
        let err = add_rule(&env.store, &form(&env, "第二条", "dev\\two")).unwrap_err();
        assert!(err.to_string().contains("已存在指向同一路径的规则"), "{err}");
        let _ = fs::remove_dir_all(&env.base);
    }
}
