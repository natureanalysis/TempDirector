//! 开机自启：注册一个**用户级登录任务**（不需要管理员权限）。
//!
//! ## 为什么不放进 `HKCU\Run`
//!
//! 我们要跑在目标软件之前。`HKCU\Run` 里的条目由 Explorer 依次拉起，顺序不可控，
//! 而任务计划的登录触发器会在 Run 键之前派发，并且可以用 `<Priority>` 提前。
//!
//! ## 为什么不用注册表/服务
//!
//! * 服务（SYSTEM）：要管理员安装，而且要在用户上下文里建目录得再套一层令牌，
//!   属于 M4 的范畴；本轮先给出**今天就能用**的方案。
//! * 用户级任务：`InteractiveToken` + `LeastPrivilege`，
//!   注册/查询/删除都不需要提权（已实测），而且只在该用户登录时跑，最贴合场景。
//!
//! ## 已知边界
//!
//! * debug 构建的 exe 带控制台，登录时那一下会闪一个黑窗；
//!   release 构建 `windows_subsystem = "windows"`，完全静默。
//! * 本任务只是"尽早"，不保证 100% 早于所有自启软件。真正的强保证（登录事件 + 用户令牌）
//!   在 M4 的服务里。做不到时也不会静默失败 —— 重建结果会落库并在界面顶部提示。

use std::path::Path;
use std::process::Command;

use serde::Serialize;

use crate::error::{TdError, TdResult};
use crate::i18n::{t, tf};

/// 任务名（平铺，不带文件夹 —— 避免 `schtasks` 里 `\` 路径的引号地狱）。
pub const TASK_NAME: &str = "TempDirector-BootRebuild";

/// 高权限登录任务：只重建 `C:\Windows` 这接受保护前缀下的链接。
///
/// 普通任务跑在 `LeastPrivilege` 下，碰不到这些目录；而 `HighestAvailable`
/// 只有在用户属于 Administrators 组时才会真正提权，否则退化为普通权限
/// （重建结果里会明确告警，见 `boot.not_elevated`）。
pub const ELEVATED_TASK_NAME: &str = "TempDirector-BootRebuild-Elevated";

/// 登录任务种类。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskKind {
    /// 普通权限任务：处理普通源（参数带 `--skip-elevated`）。
    Normal,
    /// 最高权限任务：只处理受保护源（参数带 `--elevated-only`）。
    Elevated,
}

impl TaskKind {
    pub const ALL: [Self; 2] = [Self::Normal, Self::Elevated];

    pub fn task_name(self) -> &'static str {
        match self {
            Self::Normal => TASK_NAME,
            Self::Elevated => ELEVATED_TASK_NAME,
        }
    }

    fn run_level(self) -> &'static str {
        match self {
            Self::Normal => "LeastPrivilege",
            Self::Elevated => "HighestAvailable",
        }
    }

    /// 追加在 `boot-rebuild` 之后的分工参数。
    fn boot_flag(self) -> &'static str {
        match self {
            Self::Normal => "--skip-elevated",
            Self::Elevated => "--elevated-only",
        }
    }

    fn desc(self) -> String {
        match self {
            Self::Normal => t("autostart.task_desc").to_string(),
            Self::Elevated => t("autostart.task_desc_elevated").to_string(),
        }
    }

    fn xml_name(self) -> &'static str {
        match self {
            Self::Normal => "boot-rebuild.task.xml",
            Self::Elevated => "boot-rebuild-elevated.task.xml",
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct AutostartStatus {
    pub registered: bool,
    pub task_name: String,
    /// 从任务里读回来的命令行（`<Command>` + `<Arguments>`）。
    pub command: String,
    pub message: String,
    /// 高权限登录任务（只处理受保护源）。
    pub elevated_registered: bool,
    pub elevated_task_name: String,
    pub elevated_command: String,
    pub elevated_message: String,
    /// 已注册任务的参数与当前版本不一致（旧版没有分工参数），建议重新启用。
    pub needs_update: bool,
}

fn no_window(cmd: &mut Command) -> &mut Command {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    cmd.creation_flags(CREATE_NO_WINDOW)
}

/// 调用 `schtasks`。返回 `(exit_code, 合并输出)`。
fn schtasks(args: &[&str]) -> TdResult<(i32, String)> {
    let mut cmd = Command::new("schtasks");
    cmd.args(args);
    no_window(&mut cmd);
    let out = cmd
        .output()
        .map_err(|e| TdError::Other(tf("autostart.schtasks_failed", &[("err", &e.to_string())])))?;
    let mut bytes = out.stdout.clone();
    bytes.extend_from_slice(&out.stderr);
    Ok((out.status.code().unwrap_or(-1), decode(&bytes)))
}

/// `schtasks /XML` 可能输出 UTF-16LE，普通查询输出 OEM 码页 —— 两种都要能解。
fn decode(bytes: &[u8]) -> String {
    let has_bom = bytes.len() >= 2 && bytes[0] == 0xFF && bytes[1] == 0xFE;
    let utf16_like =
        bytes.len() >= 4 && (has_bom || (bytes[1] == 0x00 && bytes[3] == 0x00));
    if utf16_like {
        // 必须跳过 BOM，否则解出来的字符串会以一个不可见的 U+FEFF 开头
        let start = if has_bom { 2 } else { 0 };
        let u16s: Vec<u16> = bytes[start..]
            .chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .collect();
        String::from_utf16_lossy(&u16s)
    } else {
        String::from_utf8_lossy(bytes).to_string()
    }
}

fn encode_utf16le_bom(text: &str) -> Vec<u8> {
    let mut out = vec![0xFF, 0xFE];
    for u in text.encode_utf16() {
        out.extend_from_slice(&u.to_le_bytes());
    }
    out
}

/// 当前用户（`DOMAIN\user`）。拿不到时返回 `None`，任务会退化成"任意用户登录"。
fn current_user_id() -> Option<String> {
    let user = std::env::var("USERNAME").ok().filter(|s| !s.is_empty())?;
    match std::env::var("USERDOMAIN") {
        Ok(domain) if !domain.is_empty() => Some(format!("{domain}\\{user}")),
        _ => Some(user),
    }
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// 生成任务 XML。`extra_args` 会追加在 `boot-rebuild <分工参数>` 之后（便携模式带数据目录）。
fn task_xml(exe: &Path, extra_args: &str, kind: TaskKind) -> TdResult<String> {
    let exe_s = exe.to_string_lossy();
    if exe_s.trim().is_empty() {
        return Err(TdError::InvalidPath(t("autostart.need_exe").into()));
    }
    let user = current_user_id();
    let user_el = match &user {
        Some(u) => format!("<UserId>{}</UserId>", xml_escape(u)),
        None => String::new(),
    };
    let mut args = format!("boot-rebuild {}", kind.boot_flag());
    if !extra_args.trim().is_empty() {
        args.push(' ');
        args.push_str(extra_args.trim());
    }
    Ok(format!(
        r#"<?xml version="1.0" encoding="UTF-16"?>
<Task version="1.2" xmlns="http://schemas.microsoft.com/windows/2004/02/mit/task">
  <RegistrationInfo>
    <Description>{desc}</Description>
    <Author>{author}</Author>
  </RegistrationInfo>
  <Triggers>
    <LogonTrigger>
      <Enabled>true</Enabled>
      {user_el}
    </LogonTrigger>
  </Triggers>
  <Principals>
    <Principal id="Author">
      {user_el}
      <LogonType>InteractiveToken</LogonType>
      <RunLevel>{run_level}</RunLevel>
    </Principal>
  </Principals>
  <Settings>
    <MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy>
    <DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries>
    <StopIfGoingOnBatteries>false</StopIfGoingOnBatteries>
    <AllowHardTerminate>true</AllowHardTerminate>
    <StartWhenAvailable>false</StartWhenAvailable>
    <RunOnlyIfNetworkAvailable>false</RunOnlyIfNetworkAvailable>
    <AllowStartOnDemand>true</AllowStartOnDemand>
    <Enabled>true</Enabled>
    <Hidden>false</Hidden>
    <RunOnlyIfIdle>false</RunOnlyIfIdle>
    <WakeToRun>false</WakeToRun>
    <ExecutionTimeLimit>PT10M</ExecutionTimeLimit>
    <Priority>4</Priority>
  </Settings>
  <Actions Context="Author">
    <Exec>
      <Command>{exe}</Command>
      <Arguments>{args}</Arguments>
    </Exec>
  </Actions>
</Task>
"#,
        author = xml_escape(&user.clone().unwrap_or_else(|| "-".into())),
        desc = xml_escape(&kind.desc()),
        run_level = kind.run_level(),
        exe = xml_escape(&exe_s),
        args = xml_escape(&args),
    ))
}

/// 单个任务的注册状态。
#[derive(Debug, Clone)]
struct TaskStatus {
    registered: bool,
    command: String,
    message: String,
}

fn task_status(kind: TaskKind) -> TaskStatus {
    let name = kind.task_name();
    let mut st = TaskStatus {
        registered: false,
        command: String::new(),
        message: String::new(),
    };

    let (code, _) = match schtasks(&["/Query", "/TN", name]) {
        Ok(v) => v,
        Err(e) => {
            st.message = e.to_string();
            return st;
        }
    };
    if code != 0 {
        st.message = t("autostart.unregistered").into();
        return st;
    }
    st.registered = true;

    // 用 /XML 取命令行：不依赖系统语言（文本查询的字段名是本地化的）
    if let Ok((0, xml)) = schtasks(&["/Query", "/TN", name, "/XML"]) {
        let cmd = extract_tag(&xml, "Command").unwrap_or_default();
        let args = extract_tag(&xml, "Arguments").unwrap_or_default();
        st.command = format!("{} {}", cmd, args).trim().to_string();
    }
    st.message = if st.command.is_empty() {
        t("autostart.registered").into()
    } else {
        tf("autostart.registered_cmd", &[("cmd", &st.command)])
    };
    st
}

/// 查询两个登录任务当前是否已注册。
pub fn status() -> AutostartStatus {
    let normal = task_status(TaskKind::Normal);
    let elevated = task_status(TaskKind::Elevated);
    let needs_update = (normal.registered && !normal.command.contains(TaskKind::Normal.boot_flag()))
        || (elevated.registered && !elevated.command.contains(TaskKind::Elevated.boot_flag()));
    AutostartStatus {
        registered: normal.registered,
        task_name: TaskKind::Normal.task_name().to_string(),
        command: normal.command,
        message: normal.message,
        elevated_registered: elevated.registered,
        elevated_task_name: TaskKind::Elevated.task_name().to_string(),
        elevated_command: elevated.command,
        elevated_message: elevated.message,
        needs_update,
    }
}

fn extract_tag(xml: &str, tag: &str) -> Option<String> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let start = xml.find(&open)? + open.len();
    let end = xml[start..].find(&close)? + start;
    Some(xml[start..end].trim().to_string())
}

/// 注册（幂等覆盖）。`extra_args` 追加在 `boot-rebuild --skip-elevated` 之后
/// （便携模式带数据目录用）。返回注册后的命令行描述。
pub fn enable(exe: &Path, extra_args: &str) -> TdResult<String> {
    enable_kind(TaskKind::Normal, exe, extra_args)
}

/// 注册高权限登录任务（只处理受保护源）。需要管理员权限。
pub fn enable_elevated(exe: &Path, extra_args: &str) -> TdResult<String> {
    enable_kind(TaskKind::Elevated, exe, extra_args)
}

fn enable_kind(kind: TaskKind, exe: &Path, extra_args: &str) -> TdResult<String> {
    crate::paths::ensure_dirs()
        .map_err(|e| TdError::Other(tf("autostart.mkdir_failed", &[("err", &e.to_string())])))?;
    let xml = task_xml(exe, extra_args, kind)?;
    let xml_path = crate::paths::data_dir().join(kind.xml_name());
    std::fs::write(&xml_path, encode_utf16le_bom(&xml))
        .map_err(|e| TdError::Other(tf("autostart.write_xml_failed", &[("err", &e.to_string())])))?;

    let xml_arg = xml_path.to_string_lossy().to_string();
    let (code, text) = schtasks(&["/Create", "/TN", kind.task_name(), "/XML", &xml_arg, "/F"])?;
    let _ = std::fs::remove_file(&xml_path);

    if code != 0 {
        return Err(TdError::Other(tf("autostart.register_failed", &[
            ("code", &code.to_string()),
            ("text", text.trim()),
        ])));
    }
    let mut line = format!("{} boot-rebuild {}", exe.display(), kind.boot_flag());
    if !extra_args.trim().is_empty() {
        line.push(' ');
        line.push_str(extra_args.trim());
    }
    Ok(line)
}

/// 取消注册（普通任务）。
pub fn disable() -> TdResult<String> {
    disable_kind(TaskKind::Normal)
}

/// 取消注册高权限任务。需要管理员权限。
pub fn disable_elevated() -> TdResult<String> {
    disable_kind(TaskKind::Elevated)
}

fn disable_kind(kind: TaskKind) -> TdResult<String> {
    let (code, text) = schtasks(&["/Delete", "/TN", kind.task_name(), "/F"])?;
    if code != 0 {
        // 本来就不存在也算成功（幂等）
        if text.contains("cannot find") || text.contains("找不到") {
            return Ok(t("autostart.not_registered").into());
        }
        return Err(TdError::Other(tf("autostart.delete_failed", &[
            ("code", &code.to_string()),
            ("text", text.trim()),
        ])));
    }
    Ok(t("autostart.disabled").into())
}

/// 立刻手动触发一次（等价于"登录时那次"），用于验证任务是否配好。
pub fn run_now() -> TdResult<String> {
    run_now_kind(TaskKind::Normal)
}

fn run_now_kind(kind: TaskKind) -> TdResult<String> {
    let (code, text) = schtasks(&["/Run", "/TN", kind.task_name()])?;
    if code != 0 {
        return Err(TdError::Other(tf("autostart.trigger_failed", &[
            ("code", &code.to_string()),
            ("text", text.trim()),
        ])));
    }
    Ok(t("autostart.triggered").into())
}

/// 挑一个合适的可执行文件去注册：优先同目录下的 GUI（release 下无控制台窗口），
/// 其次自己。
pub fn preferred_exe() -> std::path::PathBuf {
    if let Ok(me) = std::env::current_exe() {
        if let Some(dir) = me.parent() {
            let gui = dir.join("tempdirector-gui.exe");
            if gui.is_file() {
                return gui;
            }
        }
        return me;
    }
    std::path::PathBuf::from("tempdirector-gui.exe")
}

/// 便携模式（数据目录来自 `TEMPDIRECTOR_DATA_DIR`）下，登录任务**必须**显式带上数据目录。
///
/// 任务计划的动作不会继承我们 shell 里的环境变量（提权/计划任务都会重建环境块），
/// 不带上就会变成"注册时用的是 A 库、登录时跑的是 B 库"。
pub fn extra_args_from_env() -> String {
    match std::env::var(crate::paths::DATA_DIR_ENV) {
        Ok(v) if !v.trim().is_empty() => {
            format!("--data-dir {}", crate::elevate::quote_arg(v.trim()))
        }
        _ => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn task_xml_contains_logon_trigger_and_action() {
        let xml = task_xml(
            Path::new(r"C:\Program Files\TempDirector\tempdirector-gui.exe"),
            "",
            TaskKind::Normal,
        )
        .unwrap();
        assert!(xml.contains("<LogonTrigger>"));
        assert!(xml.contains("<LogonType>InteractiveToken</LogonType>"));
        assert!(xml.contains("<RunLevel>LeastPrivilege</RunLevel>"));
        assert!(xml.contains(r"<Command>C:\Program Files\TempDirector\tempdirector-gui.exe</Command>"));
        assert!(xml.contains("<Arguments>boot-rebuild --skip-elevated</Arguments>"), "{xml}");
    }

    #[test]
    fn elevated_task_uses_highest_available_and_only_protected_scope() {
        let xml = task_xml(Path::new(r"C:\x\gui.exe"), "", TaskKind::Elevated).unwrap();
        assert!(xml.contains("<RunLevel>HighestAvailable</RunLevel>"), "{xml}");
        assert!(xml.contains("<Arguments>boot-rebuild --elevated-only</Arguments>"), "{xml}");
        assert!(!xml.contains("LeastPrivilege"), "{xml}");
    }

    #[test]
    fn task_xml_appends_extra_args() {
        let xml = task_xml(Path::new(r"C:\x\gui.exe"), r#"--data-dir "D:\portable""#, TaskKind::Normal)
            .unwrap();
        // 引号在 XML 元素内容里不需要转义，schtasks 会原样作为命令行交给程序
        assert!(
            xml.contains(r#"<Arguments>boot-rebuild --skip-elevated --data-dir "D:\portable"</Arguments>"#),
            "{xml}"
        );
    }

    #[test]
    fn task_xml_escapes_ampersand() {
        let xml = task_xml(Path::new(r"C:\A & B\gui.exe"), "", TaskKind::Normal).unwrap();
        assert!(xml.contains("A &amp; B"), "{xml}");
        assert!(!xml.contains("A & B\\gui.exe"));
    }

    #[test]
    fn xml_is_valid_utf16_with_bom() {
        let xml = task_xml(Path::new(r"C:\x\gui.exe"), "", TaskKind::Normal).unwrap();
        let bytes = encode_utf16le_bom(&xml);
        assert_eq!(&bytes[..2], &[0xFF, 0xFE]);
        assert_eq!(decode(&bytes), xml);
    }

    #[test]
    fn extract_tag_works() {
        let xml = "<Exec><Command>C:\\a\\b.exe</Command><Arguments>boot-rebuild</Arguments></Exec>";
        assert_eq!(extract_tag(xml, "Command").unwrap(), r"C:\a\b.exe");
        assert_eq!(extract_tag(xml, "Arguments").unwrap(), "boot-rebuild");
        assert!(extract_tag(xml, "Missing").is_none());
    }

    #[test]
    fn status_does_not_panic() {
        let st = status();
        assert_eq!(st.task_name, TASK_NAME);
        assert_eq!(st.elevated_task_name, ELEVATED_TASK_NAME);
    }
}
