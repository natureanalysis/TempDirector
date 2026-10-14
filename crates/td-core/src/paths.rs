//! 应用路径解析。
//!
//! 数据库放在 `%PROGRAMDATA%\TempDirector\` 而不是 `%APPDATA%`：
//! 后台服务以 SYSTEM 身份运行，读不到（也不该读到）某个用户的 `%APPDATA%`。
//! 多用户场景下规则是共享的，路径里的 `%APPDATA%` 这类变量在**执行时按当前用户展开**。

use std::path::PathBuf;

use windows_sys::Win32::System::Com::CoTaskMemFree;
use windows_sys::Win32::UI::Shell::SHGetKnownFolderPath;
use windows_sys::core::GUID;

/// 显式覆盖数据目录（便携模式 / 自动化测试 / 沙箱环境）。
pub const DATA_DIR_ENV: &str = "TEMPDIRECTOR_DATA_DIR";

/// `FOLDERID_ProgramData` = {62AB5D82-FDC1-4DC3-A9DD-070D1D495D97}
const FOLDERID_PROGRAM_DATA: GUID = GUID {
    data1: 0x62AB_5D82,
    data2: 0xFDC1,
    data3: 0x4DC3,
    data4: [0xA9, 0xDD, 0x07, 0x0D, 0x1D, 0x49, 0x5D, 0x97],
};

/// 通过 `SHGetKnownFolderPath` 解析系统已知文件夹。
fn known_folder(fid: &GUID) -> Option<PathBuf> {
    unsafe {
        let mut p: *mut u16 = std::ptr::null_mut();
        let hr = SHGetKnownFolderPath(fid, 0, std::ptr::null_mut(), &mut p);
        if hr < 0 || p.is_null() {
            return None;
        }
        let mut len = 0usize;
        while *p.add(len) != 0 {
            len += 1;
        }
        let s = String::from_utf16_lossy(std::slice::from_raw_parts(p, len));
        CoTaskMemFree(p as *const core::ffi::c_void);
        Some(PathBuf::from(s))
    }
}

/// 数据根目录。
///
/// 优先级：`TEMPDIRECTOR_DATA_DIR` → `%PROGRAMDATA%` 环境变量 → `SHGetKnownFolderPath`
/// → 当前目录下的 `.tddata`（最后兜底，保证任何环境下都能起来）。
///
/// 把环境变量放在 API 之前是有意的：这样便携模式、沙箱、CI 都能通过环境变量重定向数据根。
pub fn data_dir() -> PathBuf {
    if let Ok(v) = std::env::var(DATA_DIR_ENV) {
        if !v.trim().is_empty() {
            return PathBuf::from(v);
        }
    }
    if let Ok(v) = std::env::var("PROGRAMDATA") {
        if !v.trim().is_empty() {
            return PathBuf::from(v).join("TempDirector");
        }
    }
    if let Some(base) = known_folder(&FOLDERID_PROGRAM_DATA) {
        return base.join("TempDirector");
    }
    std::env::current_dir()
        .unwrap_or_else(|_| PathBuf::from("."))
        .join(".tddata")
}

pub fn db_path() -> PathBuf {
    data_dir().join("tempdirector.db")
}

/// 提权任务投放目录。提权 worker 只接受这个目录里的 job 文件，
/// 且只认里面的 rule_id（防"提权后变成任意路径删除器"）。
pub fn jobs_dir() -> PathBuf {
    data_dir().join("jobs")
}

pub fn log_dir() -> PathBuf {
    data_dir().join("logs")
}

/// 确保数据目录存在。
pub fn ensure_dirs() -> std::io::Result<()> {
    std::fs::create_dir_all(data_dir())?;
    std::fs::create_dir_all(jobs_dir())?;
    std::fs::create_dir_all(log_dir())?;
    Ok(())
}
