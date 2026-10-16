//! 规则 → 源/目标路径：展开环境变量、收集告警、执行前校验。

use std::path::{Path, PathBuf};

use crate::error::{TdError, TdResult};
use crate::link::detect::LinkType;
use crate::store::{Root, Rule, Store};
use crate::util;

#[derive(Debug, Clone)]
pub struct Resolved {
    pub root: Root,
    pub source: PathBuf,
    pub target: PathBuf,
    pub warnings: Vec<String>,
    pub unknown_vars: Vec<String>,
}

/// 解析一条规则的源与目标（展开环境变量）。
pub fn resolve(store: &Store, rule: &Rule) -> TdResult<Resolved> {
    let root = store.get_root(rule.target_root_id)?;
    let (source, mut unknown) = rule.resolved_source();
    let (target, unk2) = rule.resolved_target(&root);
    unknown.extend(unk2);

    let mut warnings = Vec::new();
    if !unknown.is_empty() {
        warnings.push(format!(
            "存在未解析的环境变量：{}（该规则可能属于其他用户）",
            unknown.join("、")
        ));
    }
    let pf = crate::safety::preflight_source(&source);
    warnings.extend(pf.warnings.clone());

    Ok(Resolved {
        root,
        source,
        target,
        warnings,
        unknown_vars: unknown,
    })
}

pub fn rule_link_type(rule: &Rule) -> TdResult<LinkType> {
    LinkType::parse(&rule.link_type)
        .ok_or_else(|| TdError::InvalidPath(format!("未知的链接类型：{}", rule.link_type)))
}

/// 一条规则**真正要清理的目录**：
/// 源已是链接（联接/符号链接）→ 真实内容在 RAM 盘目标上，清目标；
/// 否则（普通目录 / 尚未链接）→ 清源。
///
/// GUI 与 CLI 共用这一条判定，避免"清哪个目录"两处各写一份而漂移。
pub fn clean_dir_for_rule(store: &Store, rule: &Rule) -> TdResult<PathBuf> {
    let resolved = resolve(store, rule)?;
    let source_is_link = std::fs::symlink_metadata(&resolved.source)
        .map(|m| m.file_type().is_symlink())
        .unwrap_or(false);
    Ok(if source_is_link {
        resolved.target
    } else {
        resolved.source
    })
}

/// 执行前的「环境就绪」校验。
pub(super) fn validate_ready(r: &Resolved) -> TdResult<()> {
    if !r.unknown_vars.is_empty() {
        return Err(TdError::InvalidPath(format!(
            "源路径里的环境变量无法解析：{}。请以目标用户的身份运行，或改用绝对路径。",
            r.unknown_vars.join("、")
        )));
    }
    let (root_path, _) = r.root.resolved_path();
    if !root_path.is_dir() {
        // 目标根可能是 RAM 盘下的子目录。RAM 盘刚重启时"盘在、根目录不在"是正常状态，
        // 直接把它建出来即可 —— 不该让用户先去点一次「开机重建」才能应用规则。
        crate::boot::ensure_root_dir(&root_path)?;
    }
    crate::safety::validate_rule(&r.source, &r.target)?;
    Ok(())
}

pub(super) fn dir_has_content(dir: &Path) -> bool {
    std::fs::read_dir(util::to_verbatim(dir))
        .map(|mut it| it.next().is_some())
        .unwrap_or(false)
}