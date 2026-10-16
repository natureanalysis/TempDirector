//! 规则与目标根的增删改用例（含表单校验、ACL 模板存取）。

use crate::boot::acl::export_sddl;
use crate::engine;
use crate::error::{TdError, TdResult};
use crate::link::LinkType;
use crate::store::{Store, CATEGORIES};

use super::view::RuleForm;

pub fn add_root(
    store: &Store,
    label: &str,
    path: &str,
    is_ramdisk: bool,
    wait_timeout_ms: i64,
) -> TdResult<i64> {
    if label.trim().is_empty() {
        return Err(TdError::InvalidPath("名称不能为空".into()));
    }
    if path.trim().is_empty() {
        return Err(TdError::InvalidPath("路径不能为空，例如 R:".into()));
    }
    store.ensure_root(label, path, is_ramdisk, wait_timeout_ms)
}

pub fn delete_root(store: &Store, id: i64) -> TdResult<()> {
    store.delete_root(id)
}

pub fn add_rule(store: &Store, form: &RuleForm) -> TdResult<i64> {
    validate_form(form)?;
    store.insert_rule(&form.to_new_rule())
}

pub fn update_rule(store: &Store, id: i64, form: &RuleForm) -> TdResult<()> {
    validate_form(form)?;
    store.update_rule(id, &form.to_new_rule())
}

pub(crate) fn validate_form(form: &RuleForm) -> TdResult<()> {
    if form.name.trim().is_empty() {
        return Err(TdError::InvalidPath("规则名称不能为空".into()));
    }
    if form.source_template.trim().is_empty() {
        return Err(TdError::InvalidPath("源路径不能为空".into()));
    }
    if !CATEGORIES.contains(&form.category.as_str()) {
        return Err(TdError::InvalidPath(format!(
            "未知分类 {}，可用：{}",
            form.category,
            CATEGORIES.join(" / ")
        )));
    }
    if form.target_subpath.trim().is_empty() {
        return Err(TdError::InvalidPath(
            "目标子目录不能为空（它是 RAM 盘下的相对路径，例如 browsers\\chrome）".into(),
        ));
    }
    if LinkType::parse(&form.link_type).is_none() {
        return Err(TdError::InvalidPath(
            "链接类型只能是 junction 或 symlink_dir".into(),
        ));
    }
    // 排除模式的结构校验（空/超长/超量/..）由 Excludes 统一给出。
    crate::fsops::exclude::Excludes::new(&form.excludes)?;
    // 水位预授权策略的格式校验（empty / older:<秒>）。
    super::water::validate_auto_policy(&form.auto_clean_policy)?;
    Ok(())
}

pub fn delete_rules(store: &Store, ids: &[i64]) -> TdResult<usize> {
    let mut n = 0;
    for id in ids {
        store.delete_rule(*id)?;
        n += 1;
    }
    Ok(n)
}

pub fn set_enabled(store: &Store, ids: &[i64], enabled: bool) -> TdResult<usize> {
    for id in ids {
        store.set_rule_enabled(*id, enabled)?;
    }
    Ok(ids.len())
}

/// 同一分类内调整规则顺序（GUI 拖拽排序）。
///
/// 约束：`ordered_ids` 必须是**该分类的完整顺序**（即便界面正被筛选/搜索，
/// 前端也要从全量快照组装）。原因：`set_rules_order` 只重写传入的 id，
/// 若只传筛选后的子集，子集拿 1..n、隐藏规则保留旧值，新旧交错后顺序不可预期。
/// 这条约束在这里强制校验（数量必须等于该分类规则总数），不靠调用方自觉。
pub fn reorder_rules(store: &Store, ordered_ids: &[i64]) -> TdResult<usize> {
    if ordered_ids.is_empty() {
        return Ok(0);
    }
    let mut seen = std::collections::HashSet::new();
    for id in ordered_ids {
        if !seen.insert(*id) {
            return Err(TdError::Other(format!(
                "顺序列表里 id={id} 出现了不止一次"
            )));
        }
    }
    // 先全部取出（不存在的 id 在这一步就报错），同时校验同类。
    let mut rules = Vec::with_capacity(ordered_ids.len());
    for id in ordered_ids {
        rules.push(store.get_rule(*id)?);
    }
    let category = rules[0].category.clone();
    if let Some(r) = rules.iter().find(|r| r.category != category) {
        return Err(TdError::Other(format!(
            "只能在同一分类内调整顺序：{}（#{}）属于 {}，其它属于 {}",
            r.name, r.id, r.category, category
        )));
    }
    // 完整性：列表查询按 category 分组展示，缺一条就会让被漏掉的规则顺序悬空。
    let total = store
        .list_rules()?
        .iter()
        .filter(|r| r.category == category)
        .count();
    let given = ordered_ids.len();
    if total != given {
        return Err(TdError::Other(format!(
            "顺序列表必须覆盖分类 {category} 的全部规则（{given}/{total}）"
        )));
    }
    let n = store.set_rules_order(ordered_ids)?;
    store.log_event(
        None,
        "info",
        "rules_reordered",
        &format!("调整了分类 {category} 的 {n} 条规则顺序"),
    )?;
    Ok(n)
}

/// 保存一条目录的 DACL 作为重建模板（用于需要提权的规则）。
pub fn capture_acl(store: &Store, id: i64) -> TdResult<String> {
    let rule = store.get_rule(id)?;
    let resolved = engine::resolve(store, &rule)?;
    if !resolved.source.is_dir() {
        return Err(TdError::InvalidPath(format!(
            "源目录不存在，无法导出权限：{}",
            resolved.source.display()
        )));
    }
    let sddl = export_sddl(&resolved.source)?;
    store.set_rule_flags(id, rule.rebuild_on_boot, &rule.link_type, Some(&sddl))?;
    store.log_event(
        Some(id),
        "info",
        "acl_captured",
        &format!("{}：已保存目录权限模板", rule.name),
    )?;
    Ok(sddl)
}

pub fn clear_acl(store: &Store, id: i64) -> TdResult<()> {
    let rule = store.get_rule(id)?;
    store.set_rule_flags(id, rule.rebuild_on_boot, &rule.link_type, None)
}