//! 概览用例：界面快照、目标根/规则视图、事件与操作流水、上次开机报告。

use crate::boot::{probe_volume, BootReport};
use crate::engine;
use crate::error::TdResult;
use crate::i18n::{t, tf};
use crate::paths;
use crate::store::{EventRow, Operation, Rule, Store, CATEGORIES};

use super::view::{
    Banner, RootView, RuleView, Stats, UiSnapshot, VERSION,
};

pub fn snapshot(store: &Store) -> TdResult<UiSnapshot> {
    let roots = root_views(store)?;
    let rules = rule_views(store)?;

    let mut stats = Stats {
        total: rules.len(),
        ..Default::default()
    };
    for r in &rules {
        if r.want_enabled {
            stats.enabled += 1;
        }
        if r.needs_attention {
            stats.needs_attention += 1;
        }
        match r.badge.as_str() {
            "✅" => stats.ok += 1,
            "⚠️" => stats.warn += 1,
            "❌" => stats.plain += 1,
            _ => stats.other += 1,
        }
    }

    let open_ops = store.list_open_ops()?;
    let mut banners: Vec<Banner> = Vec::new();

    // 登录任务是静默跑的：它出问题必须在这里被看到，而不是躺在日志里
    if let Some((at, summary)) = last_boot_problem(store) {
        banners.push(Banner {
            level: "warn",
            text: tf("banner.boot_problem", &[("at", &at), ("summary", &summary)]),
            action: None,
        });
    }

    let roots_ready = roots
        .iter()
        .filter(|r| r.state == "ready" || r.state == "not_created")
        .count();
    if roots.is_empty() {
        banners.push(Banner {
            level: "warn",
            text: t("banner.no_roots").into(),
            action: None,
        });
    } else if roots_ready == 0 {
        banners.push(Banner {
            level: "error",
            text: t("banner.all_roots_down").into(),
            action: None,
        });
    }
    if stats.needs_attention > 0 {
        banners.push(Banner {
            level: "warn",
            text: tf("banner.needs_attention", &[("n", &stats.needs_attention.to_string())]),
            action: None,
        });
    }
    if !open_ops.is_empty() {
        banners.push(Banner {
            level: "error",
            text: tf("banner.open_ops", &[("n", &open_ops.len().to_string())]),
            action: None,
        });
    }
    for r in roots.iter().filter(|r| r.state != "ready") {
        banners.push(Banner {
            level: "warn",
            text: tf("banner.root_error", &[
                ("id", &r.id.to_string()),
                ("path", &r.path),
                ("message", &r.message),
            ]),
            action: None,
        });
    }

    // RAM 盘水位：写满会导致软件写缓存直接失败，必须顶在概览上（只读，事件归 water_poll）。
    for w in super::water::water_levels(store)? {
        match w.level.as_str() {
            "critical" => banners.push(Banner {
                level: "error",
                text: tf("banner.disk_critical", &[
                    ("label", &w.label),
                    ("path", &w.path),
                    ("pct", &format!("{:.1}", w.used_pct)),
                    ("free", &w.free_text),
                ]),
                action: Some("sweep".into()),
            }),
            "warn" => banners.push(Banner {
                level: "warn",
                text: tf("banner.disk_warn", &[
                    ("label", &w.label),
                    ("path", &w.path),
                    ("pct", &format!("{:.1}", w.used_pct)),
                    ("free", &w.free_text),
                ]),
                action: Some("sweep".into()),
            }),
            _ => {}
        }
    }

    Ok(UiSnapshot {
        version: VERSION.to_string(),
        schema_version: store.user_version()?,
        data_dir: paths::data_dir().display().to_string(),
        db_path: paths::db_path().display().to_string(),
        categories: CATEGORIES.iter().map(|s| s.to_string()).collect(),
        roots,
        rules,
        stats,
        banners,
        open_ops,
    })
}

pub fn root_views(store: &Store) -> TdResult<Vec<RootView>> {
    let mut out = Vec::new();
    for r in store.list_roots()? {
        let (path, unknown) = r.resolved_path();
        let (state, message) = probe_volume(&path);
        out.push(RootView {
            id: r.id,
            label: r.label,
            path: r.path,
            is_ramdisk: r.is_ramdisk,
            wait_timeout_ms: r.wait_timeout_ms,
            state,
            message,
            unknown_vars: unknown,
        });
    }
    Ok(out)
}

pub fn rule_views(store: &Store) -> TdResult<Vec<RuleView>> {
    let mut out = Vec::new();
    for rule in store.list_rules()? {
        let resolved = engine::resolve(store, &rule)?;
        let status = engine::rule_status(store, &rule)?;
        out.push(merge_view(rule, resolved, status));
    }
    Ok(out)
}

fn merge_view(rule: Rule, resolved: engine::Resolved, status: engine::RuleStatus) -> RuleView {
    RuleView {
        id: rule.id,
        name: rule.name,
        category: rule.category,
        source_template: rule.source_template,
        source: resolved.source.display().to_string(),
        target: resolved.target.display().to_string(),
        target_root_id: rule.target_root_id,
        target_subpath: rule.target_subpath,
        link_type: rule.link_type,
        want_enabled: rule.want_enabled,
        rebuild_on_boot: rule.rebuild_on_boot,
        has_acl: rule.acl_sddl.is_some(),
        needs_elevation: rule.needs_elevation,
        notes: rule.notes,
        excludes: rule.excludes,
        auto_clean_policy: rule.auto_clean_policy,
        badge: status.badge.to_string(),
        label: status.label,
        needs_attention: status.needs_attention,
        warnings: status.warnings,
        unknown_vars: status.unknown_vars,
        last_files: rule.last_files,
        last_bytes: rule.last_bytes,
        applied_at: rule.applied_at,
    }
}

pub fn recent_events(store: &Store, limit: i64) -> TdResult<Vec<EventRow>> {
    store.recent_events(limit)
}

pub fn recent_ops(store: &Store, limit: i64) -> TdResult<Vec<Operation>> {
    store.recent_ops(limit)
}

/// 上一次开机重建是不是有问题（界面顶部提示用）。返回 `(时间, 摘要)`。
pub fn last_boot_problem(store: &Store) -> Option<(String, String)> {
    let json = store.get_setting("last_boot_report").ok().flatten()?;
    let report: BootReport = serde_json::from_str(&json).ok()?;
    if !report.has_problem() {
        return None;
    }
    let at = store
        .get_setting("last_boot_at")
        .ok()
        .flatten()
        .unwrap_or_default();
    Some((at, report.one_line()))
}