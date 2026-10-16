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
use crate::i18n::{t, tf};
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
            return Err(TdError::Other(tf("acl.export_failed", &[
                ("rc", &rc.to_string()),
                ("path", &path.display().to_string()),
            ])));
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
            Err(TdError::Other(tf("acl.to_sddl_failed", &[
                ("path", &path.display().to_string()),
            ])))
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
            return Err(TdError::Other(tf("acl.parse_failed", &[
                ("sddl", sddl),
            ])));
        }
        let applied = SetFileSecurityW(wide.as_ptr(), DACL_SECURITY_INFORMATION, sd);
        LocalFree(sd);
        if applied == 0 {
            let e = std::io::Error::last_os_error();
            return Err(io_ctx(e, path, t("io.action.restore_acl")));
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

    /// 探针 DACL：显式授予 Everyone 完全控制，`P` 标志切断继承。
    /// 不引用具体账号 SID，测试不依赖运行身份。
    const PROBE_SDDL: &str = "D:P(A;OICI;FA;;;WD)";

    #[test]
    fn export_apply_roundtrip() {
        let base = std::env::temp_dir().join(format!("td_acl_{}", std::process::id()));
        let src = base.join("src");
        let dst = base.join("dst");
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(&src).unwrap();
        fs::create_dir_all(&dst).unwrap();

        // %TEMP% 的继承链因机器而异：回灌不带 protected 标志时，父目录的
        // 可继承 ACE 会并入回读结果，与原始 SDDL 差一份继承条目。先用探针
        // DACL 把源目录切成"仅显式 ACE"，往返才有确定答案。
        apply_sddl(&src, PROBE_SDDL).unwrap();

        let sddl = export_sddl(&src).unwrap();
        assert!(!sddl.is_empty(), "应能导出非空 DACL");
        assert!(sddl.starts_with('D'), "SDDL 应以 D: (DACL) 开头，实际 {sddl}");

        // 贴到另一个目录上；同样以 protected 形式贴入，避免并入继承 ACE
        apply_sddl(&dst, &ensure_dacl_protected(&sddl)).unwrap();

        // 回读应一致
        let again = export_sddl(&dst).unwrap();
        assert_eq!(normalize(&sddl), normalize(&again));

        let _ = fs::remove_dir_all(&base);
    }

    /// 给 `D:(...)` 形式的 SDDL 补上 `P`（protected）标志；已带标志则原样返回。
    fn ensure_dacl_protected(sddl: &str) -> String {
        match sddl.strip_prefix("D:(") {
            Some(rest) => format!("D:P({rest}"),
            None => sddl.to_string(),
        }
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
