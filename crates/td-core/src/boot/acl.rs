//! 目录 DACL 的导出与回灌（SDDL 形式）。
//!
//! ## 为什么需要这一层
//!
//! 开机重建如果以 **SYSTEM / 管理员**身份执行，`create_dir_all` 出来的空目录
//! 属主是 `Administrators`、DACL 里没有当前用户，应用写缓存时会拿到
//! "Access Denied"，表现出来就是**软件莫名报错、缓存写不进去**。
//!
//! 解法有两条：
//! 1. **以用户身份重建**（首选）—— 目录继承父目录 ACL，天然正确，无需 SDDL。
//! 2. 确实必须提权时，把"首次重定向那一刻源目录的 DACL"存进规则库，
//!    重建后用 [`apply_sddl`] 原样贴回去。
//!
//! 这里实现的是第 2 条。SDDL 里只带 **DACL**（不带 owner/group/SACL），
//! 避免不小心改动属主或审计策略。

use std::path::Path;

use windows_sys::Win32::Foundation::LocalFree;
use windows_sys::Win32::Security::Authorization::{
    ConvertSecurityDescriptorToStringSecurityDescriptorW,
    ConvertStringSecurityDescriptorToSecurityDescriptorW, GetNamedSecurityInfoW,
};
use windows_sys::Win32::Security::SetFileSecurityW;

use crate::error::{io_ctx, TdError, TdResult};
use crate::util;

/// `SE_FILE_OBJECT`
const SE_FILE_OBJECT: i32 = 1;
/// `DACL_SECURITY_INFORMATION`
const DACL_SECURITY_INFORMATION: u32 = 0x0000_0004;
/// `SDDL_REVISION_1`
const SDDL_REVISION_1: u32 = 1;

/// 导出一个目录的 DACL（SDDL 字符串）。
pub fn export_sddl(path: &Path) -> TdResult<String> {
    let wide = util::to_wide(&util::to_verbatim(path).to_string_lossy());
    unsafe {
        let mut sd: *mut core::ffi::c_void = std::ptr::null_mut();
        let rc = GetNamedSecurityInfoW(
            wide.as_ptr(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &mut sd,
        );
        if rc != 0 || sd.is_null() {
            return Err(TdError::Other(format!(
                "导出目录权限失败（Win32 错误 {rc}）：{}",
                path.display()
            )));
        }

        let mut out: *mut u16 = std::ptr::null_mut();
        let mut len: u32 = 0;
        let ok = ConvertSecurityDescriptorToStringSecurityDescriptorW(
            sd,
            SDDL_REVISION_1,
            DACL_SECURITY_INFORMATION,
            &mut out,
            &mut len,
        );
        let result = if ok == 0 || out.is_null() {
            Err(TdError::Other(format!(
                "把权限描述转成 SDDL 失败：{}",
                path.display()
            )))
        } else {
            let mut n = 0usize;
            while *out.add(n) != 0 {
                n += 1;
            }
            Ok(String::from_utf16_lossy(std::slice::from_raw_parts(out, n)))
        };

        if !out.is_null() {
            LocalFree(out as *mut core::ffi::c_void);
        }
        LocalFree(sd);
        result
    }
}

/// 把一个 SDDL 里的 DACL 贴到目录上。
pub fn apply_sddl(path: &Path, sddl: &str) -> TdResult<()> {
    if sddl.trim().is_empty() {
        return Ok(());
    }
    let wide = util::to_wide(&util::to_verbatim(path).to_string_lossy());
    let sddl_wide = util::to_wide(sddl.trim());
    unsafe {
        let mut sd: *mut core::ffi::c_void = std::ptr::null_mut();
        let ok = ConvertStringSecurityDescriptorToSecurityDescriptorW(
            sddl_wide.as_ptr(),
            SDDL_REVISION_1,
            &mut sd,
            std::ptr::null_mut(),
        );
        if ok == 0 || sd.is_null() {
            return Err(TdError::Other(format!(
                "SDDL 解析失败（记录可能来自另一台机器，SID 已失效）：{sddl}"
            )));
        }
        let applied = SetFileSecurityW(wide.as_ptr(), DACL_SECURITY_INFORMATION, sd);
        LocalFree(sd);
        if applied == 0 {
            let e = std::io::Error::last_os_error();
            return Err(io_ctx(e, path, "回灌目录权限"));
        }
    }
    Ok(())
}

/// 目标平台不支持时给出明确说明（本工具目前仅 Windows）。
pub fn supported() -> bool {
    cfg!(windows)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn export_apply_roundtrip() {
        let base = std::env::temp_dir().join(format!("td_acl_{}", std::process::id()));
        let src = base.join("src");
        let dst = base.join("dst");
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(&src).unwrap();
        fs::create_dir_all(&dst).unwrap();

        let sddl = export_sddl(&src).unwrap();
        assert!(!sddl.is_empty(), "应能导出非空 DACL");
        assert!(sddl.starts_with('D'), "SDDL 应以 D: (DACL) 开头，实际 {sddl}");

        // 贴到另一个目录上
        apply_sddl(&dst, &sddl).unwrap();

        // 回读应一致
        let again = export_sddl(&dst).unwrap();
        assert_eq!(normalize(&sddl), normalize(&again));

        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn empty_sddl_is_noop() {
        let base = std::env::temp_dir().join(format!("td_acl_noop_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&base);
        assert!(apply_sddl(&base, "  ").is_ok());
        let _ = std::fs::remove_dir_all(&base);
    }

    fn normalize(s: &str) -> String {
        // SDDL 里 ACE 顺序可能被系统重排，这里只比较字符集合
        let mut v: Vec<char> = s.chars().collect();
        v.sort_unstable();
        v.into_iter().collect()
    }
}
