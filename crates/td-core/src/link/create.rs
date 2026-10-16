//! Junction / 符号链接的建立与拆除。
//!
//! ## 为什么默认用 Junction
//!
//! | | 目录 Junction (`/J`) | 目录符号链接 (`/D`) |
//! |---|---|---|
//! | 需要管理员权限 | **不需要** | 需要 `SeCreateSymbolicLinkPrivilege` 或开发者模式 |
//! | 跨本地卷 | 支持 | 支持 |
//! | UNC / 映射网络盘目标 | **不支持** | 支持 |
//! | 目标不存在时能否创建 | 能（只存路径字符串） | 能 |
//!
//! RAM 盘是本地卷，所以常规场景全部走 Junction —— **全程零 UAC 弹窗**。
//! 只有目标落在网络路径上时，才需要退化成目录符号链接（那时才要求提权）。

use std::fs;
use std::path::{Path, PathBuf};

use windows_sys::Win32::Foundation::{CloseHandle, GetLastError, INVALID_HANDLE_VALUE};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, GetDriveTypeW, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT,
    FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
};
use windows_sys::Win32::System::IO::DeviceIoControl;

use crate::error::{io_ctx, TdError, TdResult};
use crate::link::detect::LinkType;
use crate::util;

/// 挂载点重解析标记（目录 Junction 用的就是它）。
pub const IO_REPARSE_TAG_MOUNT_POINT: u32 = 0xA000_0003;
/// 符号链接重解析标记（文件与目录符号链接共用）。
pub const IO_REPARSE_TAG_SYMLINK: u32 = 0xA000_000C;

const FSCTL_GET_REPARSE_POINT: u32 = 0x0009_00A8;
const MAX_REPARSE_SIZE: usize = 16 * 1024;
const ERROR_NOT_A_REPARSE_POINT: u32 = 4390;
const ERROR_SHARING_VIOLATION: u32 = 32;
const ERROR_ACCESS_DENIED: u32 = 5;
const ERROR_PRIVILEGE_NOT_HELD: u32 = 1314;
const DRIVE_REMOTE: u32 = 4;
const GENERIC_READ: u32 = 0x8000_0000;

/// 读取路径自身的重解析标记。
///
/// - `Ok(Some(tag))`：是重解析点，返回标记值
/// - `Ok(None)`：不是重解析点（返回 `ERROR_NOT_A_REPARSE_POINT`）
///
/// 关键点是 `FILE_FLAG_OPEN_REPARSE_POINT` —— 没有它，打开的就是链接**指向的目标**。
pub fn read_reparse_tag(path: &Path) -> TdResult<Option<u32>> {
    let wide = util::to_wide(&util::to_verbatim(path).to_string_lossy());
    unsafe {
        let handle = CreateFileW(
            wide.as_ptr(),
            GENERIC_READ,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            std::ptr::null(),
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT,
            std::ptr::null_mut(),
        );
        if handle == INVALID_HANDLE_VALUE {
            return Err(io_ctx(
                std::io::Error::from_raw_os_error(GetLastError() as i32),
                path,
                "打开重解析点",
            ));
        }

        let mut buf = vec![0u8; MAX_REPARSE_SIZE];
        let mut returned: u32 = 0;
        let ok = DeviceIoControl(
            handle,
            FSCTL_GET_REPARSE_POINT,
            std::ptr::null(),
            0,
            buf.as_mut_ptr() as *mut core::ffi::c_void,
            MAX_REPARSE_SIZE as u32,
            &mut returned,
            std::ptr::null_mut(),
        );
        let result = if ok != 0 && returned >= 4 {
            Ok(Some(u32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]])))
        } else {
            let code = GetLastError();
            if code == ERROR_NOT_A_REPARSE_POINT {
                Ok(None)
            } else {
                Err(io_ctx(
                    std::io::Error::from_raw_os_error(code as i32),
                    path,
                    "读取重解析标记",
                ))
            }
        };
        CloseHandle(handle);
        result
    }
}

/// 路径是否带 `FILE_ATTRIBUTE_REPARSE_POINT`。
pub fn is_reparse_point(path: &Path) -> bool {
    use std::os::windows::fs::MetadataExt;
    match fs::symlink_metadata(path) {
        Ok(md) => md.file_attributes() & 0x400 != 0,
        Err(_) => false,
    }
}

/// 读取链接目标（去掉 Windows 内部 `\??\` 前缀）。
pub fn read_link_target(link: &Path) -> TdResult<PathBuf> {
    let raw = fs::read_link(link).map_err(|e| io_ctx(e, link, crate::i18n::t("io.action.read_link_target")))?;
    let s = raw.to_string_lossy();
    let cleaned = s.strip_prefix(r"\??\").unwrap_or(&s);
    Ok(util::simplify(Path::new(cleaned)))
}

/// 建链前置校验。把"会炸的输入"全部挡在动手之前。
pub fn preflight(link: &Path, target: &Path, kind: LinkType) -> TdResult<()> {
    // 1. 链接位置必须空着。symlink_metadata 对死链也能成功，所以两件事都要查。
    if link.exists() || fs::symlink_metadata(link).is_ok() {
        return Err(TdError::SafetyBlocked(format!(
            "链接位置已被占用（可能是上次失败残留），请先处理：{}",
            link.display()
        )));
    }
    if let Some(parent) = link.parent() {
        if !parent.as_os_str().is_empty() && !parent.exists() {
            return Err(TdError::InvalidPath(format!(
                "源目录的父目录不存在，请先创建：{}",
                parent.display()
            )));
        }
    }

    // 2. 目标必须已经存在、是目录、且自身不是链接（禁止套娃）。
    let tmd = fs::symlink_metadata(target).map_err(|e| {
        if e.raw_os_error() == Some(2) {
            TdError::InvalidPath(format!(
                "目标目录不存在，请先重建 RAM 盘目录结构：{}",
                target.display()
            ))
        } else {
            io_ctx(e, target, crate::i18n::t("io.action.check_target"))
        }
    })?;
    if !tmd.is_dir() {
        return Err(TdError::InvalidPath(format!(
            "目标不是目录：{}",
            target.display()
        )));
    }
    if tmd.file_type().is_symlink() {
        return Err(TdError::SafetyBlocked(format!(
            "目标自身是链接，拒绝套娃：{}",
            target.display()
        )));
    }

    // 3. 自环 / 互为祖先。
    crate::safety::preflight_pair(link, target)?;

    // 4. Junction 不能指向网络位置。
    if kind == LinkType::Junction {
        let root = util::volume_root(target).ok_or_else(|| {
            TdError::InvalidPath(format!("无法确定目标所在卷：{}", target.display()))
        })?;
        let wide = util::to_wide(&root);
        let dt = unsafe { GetDriveTypeW(wide.as_ptr()) };
        if dt == DRIVE_REMOTE {
            return Err(TdError::InvalidPath(format!(
                "Junction 不支持网络/映射盘目标（{}），请改用目录符号链接或本地卷",
                target.display()
            )));
        }
    }
    Ok(())
}

/// 建立目录链接（`link` → `target`）。
pub fn create_dir_link(link: &Path, target: &Path, kind: LinkType) -> TdResult<()> {
    preflight(link, target, kind)?;

    let link_v = util::to_verbatim(link);
    let target_v = util::to_verbatim(target);

    match kind {
        LinkType::Junction => {
            // ⚠️ junction crate 的参数顺序是 (target, junction) —— 目标目录在前，链接路径在后。
            // 传反了会得到 `os error 183`（它去已经存在的目标目录上又建了一次目录）。
            junction::create(&target_v, &link_v).map_err(|e| {
                TdError::Other(format!(
                    "创建 Junction 失败：{e}（{} → {}）",
                    link.display(),
                    target.display()
                ))
            })?;
        }
        LinkType::Symlink => {
            std::os::windows::fs::symlink_dir(&target_v, &link_v).map_err(|e| match e.raw_os_error()
            {
                Some(code) if code as u32 == ERROR_PRIVILEGE_NOT_HELD => TdError::ElevationRequired(
                    "创建目录符号链接需要管理员权限或已开启开发者模式（错误 1314）".into(),
                ),
                _ => io_ctx(e, link, crate::i18n::t("io.action.create_symlink")),
            })?;
        }
    }

    // 建后自检：回读标记与目标。
    // 这一步不能省 —— API 返回成功但实际不是一个可用的 Junction 的情况真实存在
    // （比如杀软拦截、卷不支持重解析点），必须当场发现而不是等用户软件报错。
    let tag = read_reparse_tag(link)?;
    let expect = match kind {
        LinkType::Junction => IO_REPARSE_TAG_MOUNT_POINT,
        LinkType::Symlink => IO_REPARSE_TAG_SYMLINK,
    };
    if tag != Some(expect) {
        let _ = fs::remove_dir(link);
        return Err(TdError::VerifyFailed(format!(
            "建链自检失败：重解析标记为 {tag:?}，期望 {expect:#010x}（{}）",
            link.display()
        )));
    }
    let actual = read_link_target(link)?;
    if !util::path_eq(&actual, target) {
        let _ = fs::remove_dir(link);
        return Err(TdError::VerifyFailed(format!(
            "建链自检失败：实际指向 {}，期望 {}",
            actual.display(),
            target.display()
        )));
    }
    Ok(())
}

/// 只删除链接本身，返回它原先指向的目标路径。
///
/// **绝不递归**。对目录重解析点只用 `remove_dir`，它删掉的是链接条目，
/// 目标目录的内容原封不动。`remove_dir_all` 一旦跟随重解析点，
/// 就会把 RAM 盘里的真实缓存数据整个删掉。
pub fn remove_dir_link(link: &Path) -> TdResult<PathBuf> {
    let md = fs::symlink_metadata(link)
        .map_err(|e| io_ctx(e, link, crate::i18n::t("io.action.read_link_status")))?;
    if !md.file_type().is_symlink() {
        return Err(TdError::SafetyBlocked(format!(
            "拒绝操作：{} 不是链接，而是一个真实的{}。\
             为防误删，请人工确认后再处理。",
            link.display(),
            if md.is_dir() { "目录" } else { "文件" }
        )));
    }
    let target = read_link_target(link)?;

    fs::remove_dir(link).map_err(|e| match e.raw_os_error() {
        Some(code) if code as u32 == ERROR_SHARING_VIOLATION => TdError::Busy(format!(
            "链接被进程占用，无法删除（通常是某个程序的工作目录指向这里）：{}",
            link.display()
        )),
        Some(code) if code as u32 == ERROR_ACCESS_DENIED => TdError::ElevationRequired(format!(
            "删除链接被拒绝（需要管理员权限）：{}",
            link.display()
        )),
        _ => io_ctx(e, link, crate::i18n::t("io.action.delete_link")),
    })?;
    Ok(target)
}

/// 在某个路径上创建目录（含父级），并返回是否真的新建了。
pub fn ensure_dir(path: &Path) -> TdResult<bool> {
    if path.exists() {
        return Ok(false);
    }
    fs::create_dir_all(util::to_verbatim(path)).map_err(|e| io_ctx(e, path, crate::i18n::t("io.action.create_dir")))?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_dir_is_not_reparse_point() {
        let dir = std::env::temp_dir().join("td_probe_plain");
        let _ = fs::create_dir_all(&dir);
        assert!(!is_reparse_point(&dir));
        assert_eq!(read_reparse_tag(&dir).unwrap(), None);
        let _ = fs::remove_dir(&dir);
    }

    #[test]
    fn rejects_occupied_link_slot() {
        let base = std::env::temp_dir().join("td_probe_occupied");
        let target = base.join("target");
        let link = base.join("link");
        let _ = fs::create_dir_all(&target);
        let _ = fs::create_dir_all(&link);
        let err = create_dir_link(&link, &target, LinkType::Junction).unwrap_err();
        assert!(matches!(err, TdError::SafetyBlocked(_)), "{err}");
        let _ = fs::remove_dir_all(&base);
    }
}
