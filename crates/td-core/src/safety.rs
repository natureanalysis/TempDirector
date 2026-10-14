//! 规则合法性校验（黑名单 + 自环检测）。
//!
//! 这一层的存在理由是：本工具**会移动和删除用户目录的内容**。
//! 一条写错的规则（比如把 `C:\Windows` 当成缓存目录）后果不可逆，
//! 所以所有"会不会炸"的判断都必须集中在这里，且在建链、搬运之前强制执行。

use std::path::Path;

use crate::error::{TdError, TdResult};
use crate::util;

/// 绝对禁止作为「源目录」的路径（去掉卷根后比较，精确匹配）。
const FORBIDDEN_EXACT: &[&str] = &[
    "", // 卷根本身
    "windows",
    "windows\\system32",
    "windows\\syswow64",
    "windows\\winsxs",
    "program files",
    "program files (x86)",
    "programdata",
    "users",
    "users\\public",
    "system volume information",
    "$recycle.bin",
    "recovery",
    "perflogs",
    "boot",
    "efi",
    "msocache",
    "config.msi",
];

/// 其**内部任意层级**都禁止的目录前缀。
const FORBIDDEN_PREFIX: &[&str] = &[
    "system volume information\\",
    "$recycle.bin\\",
    "boot\\",
    "efi\\",
    "recovery\\",
    "windows\\winsxs\\",
    "programdata\\microsoft\\",
];

/// 需要管理员权限才能写入的目录前缀 —— 命中则标记 `requires_elevation`。
const ELEVATED_PREFIX: &[&str] = &[
    "program files\\",
    "program files (x86)\\",
    "programdata\\",
    "windows\\",
];

#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct SourcePreflight {
    /// 命中即拒绝执行。
    pub blocking: Vec<String>,
    /// 允许执行但必须提示用户。
    pub warnings: Vec<String>,
    /// 该路径需要管理员权限才能移动/删除。
    pub requires_elevation: bool,
}

impl SourcePreflight {
    pub fn is_blocked(&self) -> bool {
        !self.blocking.is_empty()
    }
}

/// 把路径转成「去掉卷根、小写、无反斜杠开头」的相对形式，用于黑名单匹配。
fn relative_to_volume(p: &Path) -> String {
    let s = util::normalize_key(&util::simplify(p).to_string_lossy());
    // `c:\x` → 从字节 3 起切片。前三字节恒为 ASCII（盘符 + ':' + '\'），
    // 所以 `get(3..)` 在多字节路径上也是安全的字符边界。
    match s.get(3..) {
        Some(rest) => rest.trim_start_matches('\\').to_string(),
        None => s,
    }
}

fn is_volume_root(p: &Path) -> bool {
    let s = util::normalize_key(&util::simplify(p).to_string_lossy());
    // "c:\" 或 "c:" 都算卷根
    s.len() == 2 || (s.len() == 3 && s.ends_with('\\'))
}

/// 源路径体检。
pub fn preflight_source(source: &Path) -> SourcePreflight {
    let mut out = SourcePreflight::default();
    let rel = relative_to_volume(source);

    if is_volume_root(source) {
        out.blocking
            .push(format!("拒绝把整个卷根作为缓存目录：{}", source.display()));
    }
    if FORBIDDEN_EXACT.contains(&rel.as_str()) {
        out.blocking.push(format!(
            "该路径属于系统关键目录，禁止重定向：{}",
            source.display()
        ));
    }
    for prefix in FORBIDDEN_PREFIX {
        if rel.starts_with(prefix) {
            out.blocking.push(format!(
                "该路径位于系统保留区（{prefix}），禁止重定向：{}",
                source.display()
            ));
        }
    }

    if ELEVATED_PREFIX.iter().any(|p| rel.starts_with(p)) {
        out.requires_elevation = true;
        out.warnings.push(format!(
            "该路径位于受保护位置，移动/建链需要管理员权限：{}",
            source.display()
        ));
    }
    if rel.contains("onedrive") || rel.contains("dropbox") {
        out.warnings.push(
            "该路径疑似在云同步目录内：云盘客户端会用重解析点做占位符，与目录链接冲突，强烈不建议重定向"
                .to_string(),
        );
    }
    if rel.ends_with("\\temp") || rel == "temp" {
        out.warnings.push(
            "重定向整个 TEMP 目录影响面较大（安装程序、待重启的更新会依赖它），建议逐软件配置"
                .to_string(),
        );
    }
    if util::simplify(source).to_string_lossy().len() > 200 {
        out.warnings.push(
            "路径较长，部分老程序（未声明 longPathAware）仍可能受 260 字符限制影响".to_string(),
        );
    }
    out
}

/// 源路径是否需要提权。
pub fn requires_elevation(source: &Path) -> bool {
    let rel = relative_to_volume(source);
    ELEVATED_PREFIX.iter().any(|p| rel.starts_with(p)) || is_volume_root(source)
}

/// 成对校验：源与目标不能相同、不能互为祖先（否则递归展开会无限套娃）。
pub fn preflight_pair(source: &Path, target: &Path) -> TdResult<()> {
    let (s, t) = (
        util::normalize_key(&util::simplify(source).to_string_lossy()),
        util::normalize_key(&util::simplify(target).to_string_lossy()),
    );
    if s.is_empty() || t.is_empty() {
        return Err(TdError::InvalidPath("源路径与目标路径都不能为空".into()));
    }
    if !source.is_absolute() || !target.is_absolute() {
        return Err(TdError::InvalidPath(
            "源路径与目标路径必须是绝对路径（支持 %VAR% 模板，但展开后必须绝对）".into(),
        ));
    }
    if s == t {
        return Err(TdError::SafetyBlocked(format!(
            "源路径与目标路径相同：{s}"
        )));
    }
    if t.starts_with(&format!("{s}\\")) {
        return Err(TdError::SafetyBlocked(format!(
            "目标目录位于源目录内部（{t} 在 {s} 内），会形成自环"
        )));
    }
    if s.starts_with(&format!("{t}\\")) {
        return Err(TdError::SafetyBlocked(format!(
            "源目录位于目标目录内部（{s} 在 {t} 内），会形成自环"
        )));
    }
    Ok(())
}

/// 一条规则的完整安全校验（含源路径黑名单）。
pub fn validate_rule(source: &Path, target: &Path) -> TdResult<SourcePreflight> {
    preflight_pair(source, target)?;
    let pf = preflight_source(source);
    if pf.is_blocked() {
        return Err(TdError::SafetyBlocked(pf.blocking.join("；")));
    }
    Ok(pf)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn blocks_system_dirs() {
        assert!(preflight_source(&PathBuf::from(r"C:\Windows")).is_blocked());
        assert!(preflight_source(&PathBuf::from(r"C:\Windows\System32")).is_blocked());
        assert!(preflight_source(&PathBuf::from(r"C:\Program Files")).is_blocked());
        assert!(preflight_source(&PathBuf::from(r"C:\")).is_blocked());
    }

    #[test]
    fn allows_normal_cache_dirs() {
        let pf = preflight_source(&PathBuf::from(
            r"C:\Users\a\AppData\Local\Google\Chrome\User Data\Default\Cache",
        ));
        assert!(!pf.is_blocked(), "{:?}", pf.blocking);
        assert!(!pf.requires_elevation);
    }

    #[test]
    fn marks_elevation_needed() {
        let pf = preflight_source(&PathBuf::from(r"C:\Program Files\Foo\Temp"));
        assert!(pf.requires_elevation);
        assert!(!pf.is_blocked());
    }

    #[test]
    fn detects_self_loop() {
        assert!(preflight_pair(
            &PathBuf::from(r"C:\a\b"),
            &PathBuf::from(r"C:\a\b\c")
        )
        .is_err());
        assert!(preflight_pair(
            &PathBuf::from(r"C:\a\b"),
            &PathBuf::from(r"C:\a\b")
        )
        .is_err());
        assert!(preflight_pair(
            &PathBuf::from(r"C:\a\b"),
            &PathBuf::from(r"R:\x\y")
        )
        .is_ok());
    }

    #[test]
    fn relative_is_stripped() {
        assert_eq!(relative_to_volume(&PathBuf::from(r"C:\Temp")), "temp");
        assert_eq!(
            relative_to_volume(&PathBuf::from(r"C:\Program Files\x")),
            "program files\\x"
        );
    }
}
