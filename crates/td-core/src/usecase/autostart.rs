//! 开机自启用例：注册状态查询 / 启用 / 取消 / 立即执行。

use crate::autostart as autostart_svc;
use crate::elevate;
use crate::error::TdResult;
use crate::paths;

use super::view::AutostartView;

pub fn autostart_status() -> AutostartView {
    let st = autostart_svc::status();
    AutostartView {
        registered: st.registered,
        task_name: st.task_name,
        command: st.command,
        message: st.message,
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
