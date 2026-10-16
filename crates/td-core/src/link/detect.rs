//! 链接状态检测 —— 全程只读，不修改任何东西。
//!
//! ## 判定"断裂"的权威依据
//!
//! `fs::symlink_metadata(source)` 成功 ⇒ 源路径这个**条目本身**存在；
//! 再 `fs::metadata(source)`（会**跟随**链接）失败 ⇒ 目标不可达 ⇒ 断裂。
//!
//! 注意 `Path::exists()` 对**死链**返回 `false`，所以它不能用来判断"源路径在不在"，
//! 必须先看 `symlink_metadata` 再决定语义。
//!
//! ## 为什么"链接正常但目标为空"要单独成一态
//!
//! RAM 盘重启后就是这个状态：链接完全有效，但目录是空的。
//! 从"链接是否可用"看它是健康的；从"缓存是否还在"看它是异常的。
//! UI 上必须区分开，否则用户会以为一切正常，却在奇怪"为什么浏览器变慢了"。

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::link::create::{
    read_link_target, read_reparse_tag, IO_REPARSE_TAG_MOUNT_POINT, IO_REPARSE_TAG_SYMLINK,
};
use crate::util;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LinkType {
    /// 目录联接，免管理员权限。
    Junction,
    /// 目录符号链接，需要特权（或开发者模式）。
    Symlink,
}

impl LinkType {
    pub fn as_str(self) -> &'static str {
        match self {
            LinkType::Junction => "junction",
            LinkType::Symlink => "symlink_dir",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "junction" | "j" | "J" => Some(LinkType::Junction),
            "symlink_dir" | "symlink" | "d" | "D" => Some(LinkType::Symlink),
            _ => None,
        }
    }

    pub fn label_zh(self) -> &'static str {
        match self {
            LinkType::Junction => "目录联接",
            LinkType::Symlink => "目录符号链接",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TargetState {
    /// 目标路径不存在（RAM 盘未挂载 / 目录被删）。
    Missing,
    /// 目标存在但为空（RAM 盘刚重启，目录结构已重建）。
    Empty,
    /// 目标存在且有内容。
    HasContent,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum LinkState {
    /// 源路径不存在：软件没装 / 路径写错 / 被软件升级清掉了。
    SourceMissing,
    /// 普通真实目录：未托管，内容在 C 盘上。
    PlainDir,
    /// 普通文件：规则配错了。
    PlainFile,
    /// 链接有效。
    LinkOk {
        target: PathBuf,
        link_type: LinkType,
        target_state: TargetState,
        /// 实际指向与规则登记的目标不一致（被手工改过）。
        target_mismatch: bool,
    },
    /// 链接存在但目标不可达。
    LinkBroken { target: PathBuf, link_type: LinkType },
    /// 外来重解析点（OneDrive 占位符 / AppExecLink / 卷挂载点）—— 一律不动。
    ForeignReparse { tag: u32 },
    /// 权限不足（源在受保护位置，需提权）。
    AccessDenied,
    IoError { message: String },
}

impl LinkState {
    /// UI 徽标。
    pub fn badge(&self) -> &'static str {
        match self {
            LinkState::LinkOk {
                target_state: TargetState::Empty,
                ..
            } => "⚠️",
            LinkState::LinkOk { .. } => "✅",
            LinkState::LinkBroken { .. } => "⚠️",
            LinkState::ForeignReparse { .. } => "🚫",
            LinkState::PlainDir | LinkState::PlainFile => "❌",
            _ => "❔",
        }
    }

    pub fn label_zh(&self) -> String {
        match self {
            LinkState::SourceMissing => "源路径不存在".into(),
            LinkState::PlainDir => "未链接（真实目录）".into(),
            LinkState::PlainFile => "路径是文件，规则配置有误".into(),
            LinkState::LinkOk {
                target,
                link_type,
                target_state,
                target_mismatch,
            } => {
                let t = match target_state {
                    TargetState::Missing => "目标丢失",
                    TargetState::Empty => "目标为空（RAM 盘刚重启）",
                    TargetState::HasContent => "正常",
                };
                let extra = if *target_mismatch { "，且指向与登记不符" } else { "" };
                format!(
                    "{} → {}（{t}{extra}）",
                    link_type.label_zh(),
                    target.display()
                )
            }
            LinkState::LinkBroken { target, link_type } => {
                format!("{} 断裂，目标不可达：{}", link_type.label_zh(), target.display())
            }
            LinkState::ForeignReparse { tag } => {
                format!("外来重解析点（tag {tag:#010x}），非本工具管理")
            }
            LinkState::AccessDenied => "访问被拒绝（需要管理员权限）".into(),
            LinkState::IoError { message } => format!("检测出错：{message}"),
        }
    }

    /// 是否是"本工具建立的那种"链接（无论健康还是断裂）。
    pub fn is_managed_link(&self) -> bool {
        matches!(self, LinkState::LinkOk { .. } | LinkState::LinkBroken { .. })
    }

    pub fn link_type(&self) -> Option<LinkType> {
        match self {
            LinkState::LinkOk { link_type, .. } | LinkState::LinkBroken { link_type, .. } => {
                Some(*link_type)
            }
            _ => None,
        }
    }

    pub fn target(&self) -> Option<&Path> {
        match self {
            LinkState::LinkOk { target, .. } | LinkState::LinkBroken { target, .. } => Some(target),
            _ => None,
        }
    }

    /// 需要用户处理吗（用于汇总"待处理项"）。
    pub fn needs_attention(&self) -> bool {
        matches!(
            self,
            LinkState::LinkBroken { .. }
                | LinkState::ForeignReparse { .. }
                | LinkState::PlainFile
                | LinkState::AccessDenied
                | LinkState::IoError { .. }
        )
    }
}

/// 检测某条规则的源路径当前处于什么状态。
///
/// * `expected_target` —— 规则里登记的目标路径，用于发现"链接被手工改指向别处"。
pub fn detect_link(source: &Path, expected_target: Option<&Path>) -> LinkState {
    let md = match fs::symlink_metadata(source) {
        Ok(md) => md,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return LinkState::SourceMissing,
        Err(e) if e.kind() == io::ErrorKind::PermissionDenied => return LinkState::AccessDenied,
        Err(e) => {
            return LinkState::IoError {
                message: format!("读取元数据失败：{e}"),
            }
        }
    };

    // Windows 上 Rust 把**任何**带 FILE_ATTRIBUTE_REPARSE_POINT 的条目都算作 symlink，
    // 所以 is_symlink() 只是"这是重解析点"，还需要读标记才能区分具体类型。
    if !md.file_type().is_symlink() {
        return if md.is_dir() {
            LinkState::PlainDir
        } else {
            LinkState::PlainFile
        };
    }

    let tag = match read_reparse_tag(source) {
        Ok(Some(t)) => t,
        Ok(None) => {
            return LinkState::IoError {
                message: "元数据标记为重解析点，但读不到标记值".into(),
            }
        }
        Err(e) => {
            return LinkState::IoError {
                message: format!("读取重解析标记失败：{e}"),
            }
        }
    };

    let link_type = match tag {
        IO_REPARSE_TAG_MOUNT_POINT => LinkType::Junction,
        IO_REPARSE_TAG_SYMLINK => LinkType::Symlink,
        // OneDrive 占位符、AppExecLink、卷挂载点等等：只报告，绝不改动。
        other => return LinkState::ForeignReparse { tag: other },
    };

    let target = match read_link_target(source) {
        Ok(t) => t,
        Err(e) => {
            return LinkState::IoError {
                message: format!("读取链接目标失败：{e}"),
            }
        }
    };

    // 跟随链接 —— 这一步决定"断裂"。
    match fs::metadata(source) {
        Ok(_) => {
            let target_state = probe_target(&target);
            let target_mismatch = expected_target
                .map(|exp| !util::path_eq(&target, exp))
                .unwrap_or(false);
            LinkState::LinkOk {
                target,
                link_type,
                target_state,
                target_mismatch,
            }
        }
        Err(e)
            if matches!(
                e.kind(),
                io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
            ) =>
        {
            LinkState::LinkBroken { target, link_type }
        }
        Err(e) => LinkState::IoError {
            message: format!("访问链接目标失败：{e}"),
        },
    }
}

/// 只判断"有没有内容"。缓存目录动辄十万文件，全量计数太贵，
/// 需要精确大小时由 `fsops::verify::measure` 按需在后台任务里做。
fn probe_target(target: &Path) -> TargetState {
    match fs::read_dir(util::to_verbatim(target)) {
        Ok(mut it) => match it.next() {
            None => TargetState::Empty,
            Some(_) => TargetState::HasContent,
        },
        Err(_) => TargetState::Missing,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::TdError;
    use crate::link::create::{create_dir_link, remove_dir_link};

    fn fixture(tag: &str) -> PathBuf {
        let base = std::env::temp_dir().join(format!("td_detect_{tag}_{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(&base).unwrap();
        base
    }

    #[test]
    fn missing_source() {
        let base = fixture("missing");
        let st = detect_link(&base.join("nope"), None);
        assert!(matches!(st, LinkState::SourceMissing), "{st:?}");
    }

    #[test]
    fn plain_dir() {
        let base = fixture("plain");
        let d = base.join("real");
        fs::create_dir_all(d.join("sub")).unwrap();
        fs::write(d.join("sub").join("a.txt"), b"hi").unwrap();
        let st = detect_link(&d, None);
        assert!(matches!(st, LinkState::PlainDir), "{st:?}");
    }

    #[test]
    fn junction_ok_empty_and_broken() {
        let base = fixture("junction");
        let target = base.join("target");
        let link = base.join("link");
        fs::create_dir_all(&target).unwrap();

        create_dir_link(&link, &target, LinkType::Junction).unwrap();

        // 目标为空
        let st = detect_link(&link, Some(&target));
        match &st {
            LinkState::LinkOk {
                link_type,
                target_state,
                target_mismatch,
                ..
            } => {
                assert_eq!(*link_type, LinkType::Junction);
                assert_eq!(*target_state, TargetState::Empty);
                assert!(!target_mismatch);
            }
            other => panic!("期望 LinkOk，实际 {other:?}"),
        }
        assert_eq!(st.badge(), "⚠️");

        // 目标有内容
        fs::write(target.join("x.bin"), b"data").unwrap();
        let st = detect_link(&link, Some(&target));
        assert_eq!(st.badge(), "✅");

        // 目标被删 → 断裂
        fs::remove_file(target.join("x.bin")).unwrap();
        fs::remove_dir(&target).unwrap();
        let st = detect_link(&link, Some(&target));
        assert!(matches!(st, LinkState::LinkBroken { .. }), "{st:?}");
        assert!(st.needs_attention());
    }

    #[test]
    fn target_mismatch_detected() {
        let base = fixture("mismatch");
        let target = base.join("target");
        let other = base.join("other");
        let link = base.join("link");
        fs::create_dir_all(&target).unwrap();
        fs::create_dir_all(&other).unwrap();
        create_dir_link(&link, &target, LinkType::Junction).unwrap();

        let st = detect_link(&link, Some(&other));
        assert!(matches!(
            st,
            LinkState::LinkOk {
                target_mismatch: true,
                ..
            }
        ));

        remove_dir_link(&link).unwrap();
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn remove_dir_link_keeps_target_content() {
        let base = fixture("removekeep");
        let target = base.join("target");
        let link = base.join("link");
        fs::create_dir_all(&target).unwrap();
        fs::write(target.join("keep.txt"), b"KEEP").unwrap();
        create_dir_link(&link, &target, LinkType::Junction).unwrap();

        let reported = remove_dir_link(&link).unwrap();
        assert!(util::path_eq(&reported, &target));
        // 链接没了
        assert!(fs::symlink_metadata(&link).is_err());
        // 目标内容必须完好
        assert_eq!(fs::read(target.join("keep.txt")).unwrap(), b"KEEP");
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn remove_dir_link_refuses_real_dir() {
        let base = fixture("refusereal");
        let real = base.join("real");
        fs::create_dir_all(&real).unwrap();
        let err = remove_dir_link(&real).unwrap_err();
        assert!(matches!(err, TdError::SafetyBlocked(_)), "{err}");
        assert!(real.exists());
        let _ = fs::remove_dir_all(&base);
    }
}
