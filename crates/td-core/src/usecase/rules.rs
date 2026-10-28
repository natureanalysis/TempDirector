//! 规则与目标根的增删改用例（含表单校验、ACL 模板存取）。

use crate::boot::acl::export_sddl;
use crate::engine;
use crate::error::{TdError, TdResult};
use crate::i18n::{t, tf};
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
        return Err(TdError::InvalidPath(t("form.name_empty").into()));
    }
    if path.trim().is_empty() {
        return Err(TdError::InvalidPath(t("form.path_empty").into()));
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
        return Err(TdError::InvalidPath(t("form.rule_name_empty").into()));
    }
    if form.source_template.trim().is_empty() {
        return Err(TdError::InvalidPath(t("form.source_empty").into()));
    }
    if !CATEGORIES.contains(&form.category.as_str()) {
        return Err(TdError::InvalidPath(tf(
            "form.unknown_category",
            &[("cat", &form.category), ("list", &CATEGORIES.join(" / "))],
        )));
    }
    if form.target_subpath.trim().is_empty() {
        return Err(TdError::InvalidPath(t("form.subpath_empty").into()));
    }
    if LinkType::parse(&form.link_type).is_none() {
        return Err(TdError::InvalidPath(t("form.bad_link_type").into()));
    }
    // 排除模式的结构校验（空/超长/超量/..）由 Excludes 统一给出。
    crate::fsops::exclude::Excludes::new(&form.excludes)?;
    // 水位预授权策略的格式校验（empty / older:<秒>）。
    super::water::validate_auto_policy(&form.auto_clean_policy)?;
    // 文件夹级容量上限：负数没有意义；配了上限却没策略 = 到了量也无从下手
    // （策略决定"删什么"，上限只决定"什么时候删"）。
    if form.auto_clean_limit_bytes < 0 {
        return Err(TdError::InvalidPath(tf(
            "form.bad_auto_limit",
            &[("n", &form.auto_clean_limit_bytes.to_string())],
        )));
    }
    if form.auto_clean_limit_bytes > 0 && form.auto_clean_policy.trim().is_empty() {
        return Err(TdError::InvalidPath(
            t("form.auto_limit_needs_policy").into(),
        ));
    }
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

/// settings 键：界面上被隐藏的规则 id（JSON 数组）。
///
/// 隐藏是**展示偏好**，不是规则属性：它不进 `rules` 表，所以
/// 不参与开机重建 / 水位巡检 / 批量操作，也不随配置导出导入
/// （导出的是"规则"本身，不是某人当前的界面视图）。
pub const KEY_HIDDEN_RULES: &str = "hidden_rule_ids";

/// 当前被隐藏的规则 id。手工改库写坏 JSON 时按"没有隐藏"处理。
///
/// 返回值按 id 升序，便于落库稳定、也便于测试断言。
pub fn hidden_rule_ids(store: &Store) -> TdResult<Vec<i64>> {
    let ids = store
        .get_setting(KEY_HIDDEN_RULES)?
        .and_then(|s| serde_json::from_str::<Vec<i64>>(&s).ok())
        .unwrap_or_default();
    let mut set: std::collections::BTreeSet<i64> = ids.into_iter().collect();
    // 规则已被删除时，残留的 id 没有意义，顺手清掉（只读不写，写由 set_hidden 负责）。
    let alive: std::collections::HashSet<i64> = store.list_rules()?.iter().map(|r| r.id).collect();
    set.retain(|id| alive.contains(id));
    Ok(set.into_iter().collect())
}

/// 设置一批规则的隐藏态（`hidden=true` 隐藏 / `false` 恢复显示）。
///
/// 为什么整批重写而不是逐条改：状态就是一个 JSON 数组，重写最省事、最不容易
/// 出现"同一 id 出现两次"这类脏数据。写入前会剔除已删除规则的残留 id ——
/// 否则残留 id 会一直躺在 settings 里，将来新建出同号的规则会被莫名隐藏。
///
/// 返回写入后的隐藏总数。
pub fn set_hidden(store: &Store, ids: &[i64], hidden: bool) -> TdResult<usize> {
    let alive: std::collections::HashSet<i64> = store.list_rules()?.iter().map(|r| r.id).collect();
    let mut set: std::collections::BTreeSet<i64> = hidden_rule_ids(store)?.into_iter().collect();
    for id in ids {
        if !alive.contains(id) {
            continue; // 不存在的 id 静默忽略：它可能刚被别处删掉
        }
        if hidden {
            set.insert(*id);
        } else {
            set.remove(id);
        }
    }
    let list: Vec<i64> = set.into_iter().collect();
    store.set_setting(
        KEY_HIDDEN_RULES,
        &serde_json::to_string(&list).unwrap_or_else(|_| "[]".into()),
    )?;
    Ok(list.len())
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
            return Err(TdError::Other(tf(
                "form.duplicate_id",
                &[("id", &id.to_string())],
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
        return Err(TdError::Other(tf(
            "form.cross_category",
            &[
                ("name", &r.name),
                ("id", &r.id.to_string()),
                ("cat", &r.category),
                ("other", &category),
            ],
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
        return Err(TdError::Other(tf(
            "form.incomplete_order",
            &[
                ("category", &category),
                ("given", &given.to_string()),
                ("total", &total.to_string()),
            ],
        )));
    }
    let n = store.set_rules_order(ordered_ids)?;
    store.log_event(
        None,
        "info",
        "rules_reordered",
        &tf(
            "form.reordered_log",
            &[("category", &category), ("n", &n.to_string())],
        ),
    )?;
    Ok(n)
}

/// 保存一条目录的 DACL 作为重建模板（用于需要提权的规则）。
pub fn capture_acl(store: &Store, id: i64) -> TdResult<String> {
    let rule = store.get_rule(id)?;
    let resolved = engine::resolve(store, &rule)?;
    if !resolved.source.is_dir() {
        return Err(TdError::InvalidPath(tf(
            "form.acl_no_source",
            &[("path", &resolved.source.display().to_string())],
        )));
    }
    let sddl = export_sddl(&resolved.source)?;
    store.set_rule_flags(id, rule.rebuild_on_boot, &rule.link_type, Some(&sddl))?;
    store.log_event(
        Some(id),
        "info",
        "acl_captured",
        &tf("form.acl_captured_log", &[("name", &rule.name)]),
    )?;
    Ok(sddl)
}

pub fn clear_acl(store: &Store, id: i64) -> TdResult<()> {
    let rule = store.get_rule(id)?;
    store.set_rule_flags(id, rule.rebuild_on_boot, &rule.link_type, None)
}
