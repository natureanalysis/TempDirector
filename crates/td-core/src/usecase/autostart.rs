//! 开机自启用例：注册状态查询 / 启用 / 取消 / 立即执行。

use crate::autostart as autostart_svc;
use crate::elevate;
use crate::error::{TdError, TdResult};
use crate::i18n::tf;
use crate::paths;
use crate::worker;

use super::view::AutostartView;

pub fn autostart_status() -> AutostartView {
    let st = autostart_svc::status();
    AutostartView {
        registered: st.registered,
        task_name: st.task_name,
        command: st.command,
        message: st.message,
        elevated_registered: st.elevated_registered,
        elevated_task_name: st.elevated_task_name,
        elevated_command: st.elevated_command,
        elevated_message: st.elevated_message,
        needs_update: st.needs_update,
        elevated: elevate::is_elevated(),
        data_dir: paths::data_dir().display().to_string(),
        extra_args: autostart_svc::extra_args_from_env(),
        recommended_exe: autostart_svc::preferred_exe().display().to_string(),
    }
}

pub fn autostart_enable() -> TdResult<String> {
    let exe = autostart_svc::preferred_exe();
    autostart_svc::enable(&exe, &autostart_svc::extra_args_from_env())
}

pub fn autostart_disable() -> TdResult<String> {
    autostart_svc::disable()
}

pub fn autostart_run_now() -> TdResult<String> {
    autostart_svc::run_now()
}

/// 注册 / 取消"最高权限登录任务"：这两步必须管理员身份，
/// 复用一次性提权 worker（job 里依旧只有种类，没有路径）。
fn run_autostart_job(kind: &str) -> TdResult<String> {
    let job = worker::Job {
        schema: worker::JOB_VERSION,
        kind: kind.to_string(),
        rules: Vec::new(),
        dry_run: false,
        allow_merge: false,
        sha256: false,
        best_effort: false,
        boot_scope: String::new(),
        origin_pid: std::process::id(),
    };
    let job_path = worker::write_job(&job)?;
    let exe = std::env::current_exe()
        .map_err(|e| TdError::Other(tf("elevated.exe_path_failed", &[("err", &e.to_string())])))?;
    let args = elevate::worker_args(&job_path, &autostart_svc::extra_args_from_env());

    let code = elevate::run_elevated_wait(&exe, &args)?;
    let result = worker::read_result(&job_path);
    let _ = std::fs::remove_file(&job_path);
    let _ = std::fs::remove_file(worker::result_path_for(&job_path));

    match result {
        Some(r) if r.ok => Ok(r
            .items
            .first()
            .map(|i| i.message.clone())
            .unwrap_or_default()),
        Some(r) => {
            let summary = r.one_line();
            Err(TdError::Other(r.error.unwrap_or(summary)))
        }
        None => Err(TdError::Other(tf(
            "elevated.no_result",
            &[("code", &code.to_string())],
        ))),
    }
}

/// 注册高权限登录任务（会弹一次 UAC）。
pub fn autostart_elevated_enable() -> TdResult<String> {
    run_autostart_job("autostart_elevated_enable")
}

/// 取消高权限登录任务（会弹一次 UAC）。
pub fn autostart_elevated_disable() -> TdResult<String> {
    run_autostart_job("autostart_elevated_disable")
}
