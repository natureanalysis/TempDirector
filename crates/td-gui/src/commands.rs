//! `#[tauri::command]` 薄包装层。
//!
//! 这一层只做三件事：取状态锁、调用 [`crate::api`]、把错误转成字符串。
//! **不放任何业务逻辑** —— 业务逻辑全在 `api.rs`，那里可以脱离窗口做单元测试。

use std::sync::Mutex;

use tauri::{AppHandle, Emitter, Manager, State};

use td_core::store::Store;

use crate::api;

/// 全局状态。`Store` 不是 `Sync`，用 `Mutex` 包一层；
/// 命令是同步的（跑在 Tauri 的工作线程上），因此不会卡住界面线程，
/// 但同一时刻只有一个写操作在跑 —— 这正是我们想要的。
///
/// `init_error` 记录"打开真实数据库失败、已降级到内存库"的原因，
/// 由 `ui_snapshot` 作为错误横幅显示出来（不能静默降级，用户会以为一切正常）。
pub struct AppState {
    store: Mutex<Store>,
    init_error: Option<String>,
}

impl AppState {
    pub fn new(store: Store, init_error: Option<String>) -> Self {
        Self {
            store: Mutex::new(store),
            init_error,
        }
    }

    pub fn init_error(&self) -> Option<&str> {
        self.init_error.as_deref()
    }
}

/// 进度事件名（Tauri 只允许字母数字与 `-/:_`，所以用连字符）。
pub const EV_PROGRESS: &str = "td-progress";
pub const EV_DONE: &str = "td-done";

/// 取状态锁。公开是因为托盘线程也要用同一把锁。
pub fn lock_store<'a>(
    state: &'a State<'_, AppState>,
) -> Result<std::sync::MutexGuard<'a, Store>, String> {
    state.store.lock().map_err(|_| {
        "内部状态锁已损坏（前一次操作 panic 了），请重启程序".to_string()
    })
}

type R<T> = Result<T, String>;

fn map_err(e: td_core::TdError) -> String {
    e.to_string()
}

// ---------------------------------------------------------------------------
// 读
// ---------------------------------------------------------------------------

#[tauri::command]
pub fn ui_snapshot(state: State<'_, AppState>) -> R<api::UiSnapshot> {
    let mut snap = api::snapshot(&*lock_store(&state)?).map_err(map_err)?;
    if let Some(err) = state.init_error() {
        snap.banners.insert(
            0,
            api::Banner {
                level: "error",
                text: format!(
                    "数据目录初始化失败，当前运行在临时内存库（本次改动不会保存）：{err}"
                ),
            },
        );
    }
    Ok(snap)
}

#[tauri::command]
pub fn recent_events(state: State<'_, AppState>, limit: i64) -> R<Vec<td_core::store::EventRow>> {
    api::recent_events(&*lock_store(&state)?, limit).map_err(map_err)
}

#[tauri::command]
pub fn recent_ops(state: State<'_, AppState>, limit: i64) -> R<Vec<td_core::store::Operation>> {
    api::recent_ops(&*lock_store(&state)?, limit).map_err(map_err)
}

// ---------------------------------------------------------------------------
// 目标根
// ---------------------------------------------------------------------------

#[tauri::command]
pub fn add_root(
    state: State<'_, AppState>,
    label: String,
    path: String,
    is_ramdisk: bool,
    wait_timeout_ms: i64,
) -> R<i64> {
    api::add_root(&*lock_store(&state)?, &label, &path, is_ramdisk, wait_timeout_ms).map_err(map_err)
}

#[tauri::command]
pub fn delete_root(state: State<'_, AppState>, id: i64) -> R<()> {
    api::delete_root(&*lock_store(&state)?, id).map_err(map_err)
}

// ---------------------------------------------------------------------------
// 规则
// ---------------------------------------------------------------------------

#[tauri::command]
pub fn add_rule(state: State<'_, AppState>, form: api::RuleForm) -> R<i64> {
    api::add_rule(&*lock_store(&state)?, &form).map_err(map_err)
}

#[tauri::command]
pub fn update_rule(state: State<'_, AppState>, id: i64, form: api::RuleForm) -> R<()> {
    api::update_rule(&*lock_store(&state)?, id, &form).map_err(map_err)
}

#[tauri::command]
pub fn delete_rules(state: State<'_, AppState>, ids: Vec<i64>) -> R<usize> {
    api::delete_rules(&*lock_store(&state)?, &ids).map_err(map_err)
}

#[tauri::command]
pub fn set_rules_enabled(state: State<'_, AppState>, ids: Vec<i64>, enabled: bool) -> R<usize> {
    api::set_enabled(&*lock_store(&state)?, &ids, enabled).map_err(map_err)
}

#[tauri::command]
pub fn capture_acl(state: State<'_, AppState>, id: i64) -> R<String> {
    api::capture_acl(&*lock_store(&state)?, id).map_err(map_err)
}

#[tauri::command]
pub fn clear_acl(state: State<'_, AppState>, id: i64) -> R<()> {
    api::clear_acl(&*lock_store(&state)?, id).map_err(map_err)
}

#[tauri::command]
pub fn inspect_source(template: String) -> R<api::SourceInspection> {
    api::inspect_source(&template).map_err(map_err)
}

/// 内置预设（常用软件缓存目录），只返回本机实际存在的条目。
#[tauri::command]
pub fn preset_rules() -> Vec<api::PresetView> {
    api::preset_rules()
}

#[tauri::command]
pub fn inspect_target(state: State<'_, AppState>, root_id: i64, subpath: String) -> R<api::TargetInspection> {
    api::inspect_target(&*lock_store(&state)?, root_id, &subpath).map_err(map_err)
}

// ---------------------------------------------------------------------------
// 应用 / 恢复（带进度事件）
// ---------------------------------------------------------------------------

#[tauri::command]
pub fn apply_rules(
    app: AppHandle,
    state: State<'_, AppState>,
    ids: Vec<i64>,
    opts: api::ApplyOpts,
) -> R<Vec<api::ItemOutcome>> {
    let guard = lock_store(&state)?;
    let app2 = app.clone();
    let out = api::apply_rules(&guard, &ids, &opts, &mut move |i, total, name, p| {
        let _ = app2.emit(
            EV_PROGRESS,
            serde_json::json!({
                "index": i, "total": total, "name": name,
                "phase": progress_phase(p),
                "detail": progress_detail(p),
            }),
        );
    });
    let ok = out.iter().filter(|o| o.ok).count();
    let failed = out.len() - ok;
    let _ = app.emit(
        EV_DONE,
        serde_json::json!({ "kind": "apply", "ok": ok, "failed": failed }),
    );
    Ok(out)
}

#[tauri::command]
pub fn restore_rules(
    app: AppHandle,
    state: State<'_, AppState>,
    ids: Vec<i64>,
    opts: api::ApplyOpts,
) -> R<Vec<api::ItemOutcome>> {
    let guard = lock_store(&state)?;
    let app2 = app.clone();
    let out = api::restore_rules(&guard, &ids, &opts, &mut move |i, total, name, p| {
        let _ = app2.emit(
            EV_PROGRESS,
            serde_json::json!({
                "index": i, "total": total, "name": name,
                "phase": progress_phase(p),
                "detail": progress_detail(p),
            }),
        );
    });
    let ok = out.iter().filter(|o| o.ok).count();
    let failed = out.len() - ok;
    let _ = app.emit(
        EV_DONE,
        serde_json::json!({ "kind": "restore", "ok": ok, "failed": failed }),
    );
    Ok(out)
}

#[tauri::command]
pub fn relink_rule(state: State<'_, AppState>, id: i64) -> R<api::ItemOutcome> {
    api::relink(&*lock_store(&state)?, id).map_err(map_err)
}

#[tauri::command]
pub fn check_locks(
    state: State<'_, AppState>,
    id: i64,
) -> R<td_core::fsops::occupancy::LockReport> {
    api::check_locks(&*lock_store(&state)?, id).map_err(map_err)
}

// ---------------------------------------------------------------------------
// 开机重建 / 迁移 / 其它
// ---------------------------------------------------------------------------

#[tauri::command]
pub fn boot_rebuild(app: AppHandle, state: State<'_, AppState>) -> R<td_core::boot::BootReport> {
    let guard = lock_store(&state)?;
    let report = api::boot_rebuild(&guard).map_err(map_err)?;
    let _ = app.emit(
        EV_DONE,
        serde_json::json!({ "kind": "boot", "ok": report.ok_count(), "failed": report.failed.len() }),
    );
    Ok(report)
}

#[tauri::command]
pub fn export_config(state: State<'_, AppState>, path: String) -> R<(usize, usize)> {
    api::export_config(&*lock_store(&state)?, std::path::Path::new(&path)).map_err(map_err)
}

#[tauri::command]
pub fn import_config(
    state: State<'_, AppState>,
    path: String,
    ram_root: Option<String>,
    dry_run: bool,
) -> R<td_core::portable::ImportReport> {
    api::import_config(
        &*lock_store(&state)?,
        std::path::Path::new(&path),
        ram_root.as_deref(),
        dry_run,
    )
    .map_err(map_err)
}

/// 系统目录选择框（走 Tauri dialog 插件的 Rust API，前端不依赖 npm 包）。
#[tauri::command]
pub fn pick_folder(app: AppHandle, title: String, default_path: Option<String>) -> R<Option<String>> {
    use tauri_plugin_dialog::DialogExt;
    let mut fb = app.dialog().file().set_title(&title);
    if let Some(p) = default_path.filter(|p| !p.trim().is_empty()) {
        let path = std::path::PathBuf::from(&p);
        if path.is_dir() {
            fb = fb.set_directory(path);
        }
    }
    Ok(fb
        .blocking_pick_folder()
        .and_then(|fp| fp.into_path().ok())
        .map(|p| p.display().to_string()))
}

#[tauri::command]
pub fn pick_save_file(app: AppHandle, title: String, default_name: String) -> R<Option<String>> {
    use tauri_plugin_dialog::DialogExt;
    Ok(app
        .dialog()
        .file()
        .set_title(&title)
        .set_file_name(&default_name)
        .add_filter("JSON 配置", &["json"])
        .blocking_save_file()
        .and_then(|fp| fp.into_path().ok())
        .map(|p| p.display().to_string()))
}

#[tauri::command]
pub fn pick_open_file(app: AppHandle, title: String) -> R<Option<String>> {
    use tauri_plugin_dialog::DialogExt;
    Ok(app
        .dialog()
        .file()
        .set_title(&title)
        .add_filter("JSON 配置", &["json"])
        .blocking_pick_file()
        .and_then(|fp| fp.into_path().ok())
        .map(|p| p.display().to_string()))
}

/// 在资源管理器里定位（用系统自带 explorer，不引额外依赖）。
#[tauri::command]
pub fn reveal_in_explorer(path: String) -> R<()> {
    let p = std::path::Path::new(&path);
    let target = if p.is_dir() {
        p.to_path_buf()
    } else {
        match p.parent() {
            Some(parent) if parent.is_dir() => parent.to_path_buf(),
            _ => return Err(format!("路径不存在，无法打开：{path}")),
        }
    };
    std::process::Command::new("explorer")
        .arg(target.as_os_str())
        .spawn()
        .map_err(|e| format!("调起资源管理器失败：{e}"))?;
    Ok(())
}

#[tauri::command]
pub fn show_main_window(app: AppHandle) -> R<()> {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.show();
        let _ = w.unminimize();
        let _ = w.set_focus();
    }
    Ok(())
}

#[tauri::command]
pub fn quit_app(app: AppHandle) -> R<()> {
    app.exit(0);
    Ok(())
}

// ---------------------------------------------------------------------------
// 开机自启 / 提权（M3）
// ---------------------------------------------------------------------------

#[tauri::command]
pub fn autostart_status() -> api::AutostartView {
    api::autostart_status()
}

#[tauri::command]
pub fn autostart_enable() -> R<String> {
    api::autostart_enable().map_err(map_err)
}

#[tauri::command]
pub fn autostart_disable() -> R<String> {
    api::autostart_disable().map_err(map_err)
}

#[tauri::command]
pub fn autostart_run_now() -> R<String> {
    api::autostart_run_now().map_err(map_err)
}

/// 走提权 worker 执行 apply / restore（会弹一次 UAC）。
#[tauri::command]
pub fn run_elevated_job(
    app: AppHandle,
    state: State<'_, AppState>,
    kind: String,
    ids: Vec<i64>,
    opts: api::ApplyOpts,
) -> R<Vec<api::ItemOutcome>> {
    let guard = lock_store(&state)?;
    let out = api::run_elevated_job(&guard, &kind, &ids, &opts).map_err(map_err)?;
    let ok = out.iter().filter(|o| o.ok).count();
    let failed = out.len() - ok;
    let _ = app.emit(
        EV_DONE,
        serde_json::json!({ "kind": kind, "ok": ok, "failed": failed, "elevated": true }),
    );
    Ok(out)
}

// ---------------------------------------------------------------------------
// 进度事件的小工具
// ---------------------------------------------------------------------------

use td_core::fsops::mover::Progress;

fn progress_phase(p: &Progress) -> &'static str {
    match p {
        Progress::Enumerated { .. } => "enumerated",
        Progress::Renaming { .. } => "renaming",
        Progress::Copying { .. } => "copying",
        Progress::Verifying => "verifying",
        Progress::Deleting { .. } => "deleting",
        Progress::Linking { .. } => "linking",
        Progress::Done { .. } => "done",
    }
}

fn progress_detail(p: &Progress) -> String {
    match p {
        Progress::Enumerated { files, bytes } => format!(
            "待处理 {} 个文件 / {}",
            files,
            td_core::util::human_bytes(*bytes)
        ),
        Progress::Renaming { .. } => "同卷重命名（瞬时）".into(),
        Progress::Copying { files, bytes, .. } => format!(
            "已复制 {} 个文件 / {}",
            files,
            td_core::util::human_bytes(*bytes)
        ),
        Progress::Verifying => "正在核对数据".into(),
        Progress::Deleting { files } => format!("已删除 {files} 个文件"),
        Progress::Linking { .. } => "正在建立链接".into(),
        Progress::Done { files, bytes } => format!(
            "完成：{} 个文件 / {}",
            files,
            td_core::util::human_bytes(*bytes)
        ),
    }
}
