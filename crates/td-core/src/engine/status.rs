//! 体检：单条/批量规则状态扫描，以及写进规则库的状态机键。

use serde::Serialize;

use crate::error::TdResult;
use crate::link::detect::{detect_link, LinkState, TargetState};
use crate::store::{Rule, Store};

use super::resolve::resolve;

/// 一条规则的实时体检结果。
#[derive(Debug, Clone, Serialize)]
pub struct RuleStatus {
    pub rule_id: i64,
    pub name: String,
    pub category: String,
    pub source: String,
    pub target: String,
    pub root: String,
    pub link_type: String,
    pub want_enabled: bool,
    pub rebuild_on_boot: bool,
    /// UI 徽标（✅ / ⚠️ / ❌ / 🚫 / ❔），直接来自 `LinkState::badge()`。
    pub badge: &'static str,
    pub label: String,
    pub state: LinkState,
    pub needs_attention: bool,
    pub warnings: Vec<String>,
    pub unknown_vars: Vec<String>,
}

pub fn rule_status(store: &Store, rule: &Rule) -> TdResult<RuleStatus> {
    let r = resolve(store, rule)?;
    let state = detect_link(&r.source, Some(&r.target));
    Ok(RuleStatus {
        rule_id: rule.id,
        name: rule.name.clone(),
        category: rule.category.clone(),
        source: r.source.display().to_string(),
        target: r.target.display().to_string(),
        root: r.root.label.clone(),
        link_type: rule.link_type.clone(),
        want_enabled: rule.want_enabled,
        rebuild_on_boot: rule.rebuild_on_boot,
        badge: state.badge(),
        label: state.label_zh(),
        needs_attention: state.needs_attention(),
        warnings: r.warnings.clone(),
        unknown_vars: r.unknown_vars.clone(),
        state,
    })
}

/// 批量体检。`persist` 为真时把结果写回规则库的状态缓存。
pub fn status_all(
    store: &Store,
    category: Option<&str>,
    enabled_only: bool,
    keyword: Option<&str>,
    persist: bool,
) -> TdResult<Vec<RuleStatus>> {
    let rules = store.query_rules(category, enabled_only, keyword)?;
    let mut out = Vec::with_capacity(rules.len());
    for rule in &rules {
        let st = rule_status(store, rule)?;
        if persist {
            let err = match &st.state {
                LinkState::IoError { message } => Some(message.as_str()),
                _ => None,
            };
            store.set_rule_state(rule.id, state_key(&st.state), err)?;
        }
        out.push(st);
    }
    Ok(out)
}

/// 状态机键（写进 DB 的 `last_state`）。
pub fn state_key(state: &LinkState) -> &'static str {
    match state {
        LinkState::SourceMissing => "source_missing",
        LinkState::PlainDir => "plain_dir",
        LinkState::PlainFile => "plain_file",
        LinkState::LinkOk {
            target_state: TargetState::Empty,
            ..
        } => "link_ok_empty",
        LinkState::LinkOk { .. } => "link_ok",
        LinkState::LinkBroken { .. } => "link_broken",
        LinkState::ForeignReparse { .. } => "foreign_reparse",
        LinkState::AccessDenied => "access_denied",
        LinkState::IoError { .. } => "io_error",
    }
}