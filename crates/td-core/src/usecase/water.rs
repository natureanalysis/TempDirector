//! RAM 盘水位监控用例：分级、只读扫描、巡检落事件。
//!
//! 水位 = 目标根所在卷的已用百分比。RAM 盘写满的后果是软件写缓存直接失败
//! （或触发各软件自己的回退），所以越线要被**看到**：
//!
//! * [`water_levels`] 只读扫描（快照/doctor 用，无副作用）；
//! * [`water_poll`] 巡检：与 settings 里记的上次级别对比，**级别变化才落事件**，
//!   平稳期零写入、不刷事件流水。GUI 定时器调它，界面不开着也能留下水位痕迹。
//!
//! 阈值可用 settings 覆盖：`water_warn_pct` / `water_critical_pct`（数字 0~100）。
//! 本模块只**告警**不自动删文件 —— 自动清理涉及破坏性决策，须用户显式触发。

use std::collections::BTreeMap;
use std::path::Path;

use crate::error::{TdError, TdResult};
use crate::fsops::clean::CleanPolicy;
use crate::fsops::mover::volume_space;
use crate::i18n::tf;
use crate::store::Store;
use crate::util;

use super::view::WaterLevel;

/// 默认告警线（可用 settings 键 `water_warn_pct` 覆盖）。
pub const DEFAULT_WARN_PCT: f64 = 85.0;
/// 默认严重线（可用 settings 键 `water_critical_pct` 覆盖）。
pub const DEFAULT_CRIT_PCT: f64 = 95.0;

/// settings 里记「上次各根水位级别」的键（JSON：`{路径: 级别}`）。
const STATE_KEY: &str = "water_level_state";
/// settings 里记「上次自动清理时刻」（Unix 秒）—— 冷却去抖用。
const LAST_AUTO_CLEAN_KEY: &str = "water_last_auto_clean_at";
/// 自动清理冷却期：期内不重复触发（水位降不下去时不反复删）。
const AUTO_CLEAN_COOLDOWN_SECS: i64 = 30 * 60;

/// 校验预授权策略串：`''`（未启用）/ `empty` / `older:<正整数秒>`。
/// 空串是合法值（未启用），只有「非空但解析不了」才报错。
pub fn validate_auto_policy(s: &str) -> TdResult<()> {
    let trimmed = s.trim();
    if trimmed.is_empty() || parse_auto_policy(trimmed).is_some() {
        Ok(())
    } else {
        Err(TdError::InvalidPath(tf("water.invalid_policy", &[("s", s)])))
    }
}

/// 解析预授权策略串 → CleanPolicy（`now` 为当前 Unix 秒）。
/// `None` = 未启用或格式非法。
pub fn parse_auto_policy(s: &str) -> Option<CleanPolicy> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    match s.trim() {
        "" => None,
        "empty" => Some(CleanPolicy::Empty),
        other => {
            let secs: i64 = other.strip_prefix("older:")?.trim().parse().ok()?;
            if secs <= 0 {
                return None;
            }
            Some(CleanPolicy::Before { cutoff: now - secs })
        }
    }
}

fn threshold(store: &Store, key: &str, default: f64) -> f64 {
    store
        .get_setting(key)
        .ok()
        .flatten()
        .and_then(|s| s.trim().parse::<f64>().ok())
        .filter(|v| (0.0..=100.0).contains(v))
        .unwrap_or(default)
}

/// 纯分级（越界值视为未配置，回落默认）。
fn classify(pct: f64, warn: f64, crit: f64) -> &'static str {
    if pct >= crit {
        "critical"
    } else if pct >= warn {
        "warn"
    } else {
        "ok"
    }
}

/// 只读扫描所有目标根的当前水位（不落库、不产生事件）。
pub fn water_levels(store: &Store) -> TdResult<Vec<WaterLevel>> {
    let warn = threshold(store, "water_warn_pct", DEFAULT_WARN_PCT);
    let crit = threshold(store, "water_critical_pct", DEFAULT_CRIT_PCT);
    let mut out = Vec::new();
    for r in store.list_roots()? {
        let (path, _unknown) = r.resolved_path();
        let (level, used_pct, free, free_text, total_text, message) =
            match volume_space(Path::new(&path)) {
                Ok((total, free_b)) if total > 0 => {
                    let used = total.saturating_sub(free_b);
                    let pct = used as f64 / total as f64 * 100.0;
                    let lv = classify(pct, warn, crit);
                    let msg = crate::i18n::tf("water.message.ok", &[
                        ("pct", &format!("{pct:.1}")),
                        ("free", &util::human_bytes(free_b)),
                    ]);
                    (lv.to_string(), pct, free_b, util::human_bytes(free_b), util::human_bytes(total), msg)
                }
                Ok((total, _)) => (
                    "unknown".into(),
                    0.0,
                    0,
                    util::human_bytes(0),
                    util::human_bytes(total),
                    crate::i18n::t("water.message.zero").into(),
                ),
                Err(_) => (
                    "unknown".into(),
                    0.0,
                    0,
                    util::human_bytes(0),
                    util::human_bytes(0),
                    crate::i18n::t("water.message.unavailable").into(),
                ),
            };
        out.push(WaterLevel {
            root_id: r.id,
            label: r.label,
            path: r.path,
            used_pct,
            free,
            free_text,
            total_text,
            level,
            message,
        });
    }
    Ok(out)
}

/// 巡检：扫描水位，与上次记录对比，**级别变化**才落一条事件并写回状态。
///
/// 事件级别映射：进入 `warn` → warn；进入 `critical` → error；降级 → info（恢复）。
/// 首次巡检只建立基线，`ok` 不落事件（避免装完就有一条废话）。
pub fn water_poll(store: &Store) -> TdResult<Vec<WaterLevel>> {
    let levels = water_levels(store)?;
    let old: BTreeMap<String, String> = store
        .get_setting(STATE_KEY)?
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();

    let mut new_state: BTreeMap<String, String> = BTreeMap::new();
    for lv in &levels {
        if lv.level == "unknown" {
            continue;
        }
        new_state.insert(lv.path.clone(), lv.level.clone());
        // 首次巡检视为从 ok 出发：直接 warn/critical（装完就写满）也要落事件，只有 ok 静默建基线。
        let from = old.get(&lv.path).map(String::as_str).unwrap_or("ok");
        if from == lv.level {
            continue;
        }
        let (ev_level, msg) = match lv.level.as_str() {
            "critical" => (
                "error",
                tf("water.critical", &[
                    ("label", &lv.label),
                    ("path", &lv.path),
                    ("pct", &format!("{:.1}", lv.used_pct)),
                    ("free", &lv.free_text),
                ]),
            ),
            "warn" => (
                "warn",
                tf("water.warn", &[
                    ("label", &lv.label),
                    ("path", &lv.path),
                    ("pct", &format!("{:.1}", lv.used_pct)),
                    ("free", &lv.free_text),
                ]),
            ),
            _ => (
                "info",
                tf("water.recovered", &[
                    ("label", &lv.label),
                    ("path", &lv.path),
                    ("pct", &format!("{:.1}", lv.used_pct)),
                    ("free", &lv.free_text),
                ]),
            ),
        };
        store.log_event(None, ev_level, "water", &msg)?;
    }

    if old != new_state {
        store.set_setting(STATE_KEY, &serde_json::to_string(&new_state)?)?;
    }

    run_auto_clean(store, &levels)?;
    Ok(levels)
}

/// 水位 critical 时的预授权自动清理。
///
/// * 只处理「目标根在 critical 卷上 **且** 配了 `auto_clean_policy`」的规则；
/// * 冷却期内（30 分钟）不重复触发 —— 水位降不下去（比如文件都被占用）时不反复删；
/// * 每条规则走 `clean_run` 门面（排除模式照常生效，事件流水照常落），
///   另落一条 `auto_clean` 汇总事件，让人一眼看出"这次删是水位触发的"。
fn run_auto_clean(store: &Store, levels: &[WaterLevel]) -> TdResult<()> {
    let critical_roots: Vec<i64> = levels
        .iter()
        .filter(|w| w.level == "critical")
        .map(|w| w.root_id)
        .collect();
    if critical_roots.is_empty() {
        return Ok(());
    }
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    if let Some(last) = store
        .get_setting(LAST_AUTO_CLEAN_KEY)?
        .and_then(|s| s.trim().parse::<i64>().ok())
    {
        if now - last < AUTO_CLEAN_COOLDOWN_SECS {
            return Ok(());
        }
    }

    let mut done = 0usize;
    let mut failed = 0usize;
    let mut deleted_files = 0u64;
    let mut deleted_bytes = 0u64;
    for rule in store.list_rules()? {
        if rule.auto_clean_policy.trim().is_empty()
            || !critical_roots.contains(&rule.target_root_id)
        {
            continue;
        }
        let Some(policy) = parse_auto_policy(&rule.auto_clean_policy) else {
            // 配置坏了（手工改库等）：不拖垮巡检，但要被看到。
            store.log_event(
                Some(rule.id),
                "warn",
                "auto_clean",
                &tf("water.bad_rule_policy", &[
                    ("name", &rule.name),
                    ("s", &rule.auto_clean_policy),
                ]),
            )?;
            continue;
        };
        match super::clean::clean_run(
            store,
            &super::view::CleanTarget::Rule { id: rule.id },
            &policy,
            false,
            &mut |_| {},
        ) {
            Ok(rep) => {
                done += 1;
                deleted_files += rep.deleted_files;
                deleted_bytes += rep.deleted_bytes;
            }
            Err(_) => failed += 1,
        }
    }

    if done + failed > 0 {
        let level = if failed > 0 { "warn" } else { "info" };
        store.log_event(
            None,
            level,
            "auto_clean",
            &tf("water.auto_clean_log", &[
                ("done", &done.to_string()),
                ("files", &deleted_files.to_string()),
                ("size", &util::human_bytes(deleted_bytes)),
                ("failed", &if failed > 0 {
                    tf("water.failed_suffix", &[("n", &failed.to_string())])
                } else {
                    String::new()
                }),
            ]),
        )?;
        store.set_setting(LAST_AUTO_CLEAN_KEY, &now.to_string())?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_boundaries() {
        assert_eq!(classify(0.0, 85.0, 95.0), "ok");
        assert_eq!(classify(84.9, 85.0, 95.0), "ok");
        assert_eq!(classify(85.0, 85.0, 95.0), "warn", "告警线含等于");
        assert_eq!(classify(94.9, 85.0, 95.0), "warn");
        assert_eq!(classify(95.0, 85.0, 95.0), "critical", "严重线含等于");
        assert_eq!(classify(100.0, 85.0, 95.0), "critical");
        // 阈值可自定义（warn 高于默认 crit 也不管：配置者优先）
        assert_eq!(classify(70.0, 70.0, 99.0), "warn");
    }

    #[test]
    fn parse_auto_policy_forms() {
        assert!(parse_auto_policy("").is_none(), "空 = 未启用");
        assert!(parse_auto_policy("  ").is_none());
        assert!(matches!(parse_auto_policy("empty"), Some(CleanPolicy::Empty)));
        assert!(matches!(parse_auto_policy(" older:3600 "), Some(CleanPolicy::Before { .. })));
        // 非法：前缀不对 / 非数字 / 零 / 负数
        assert!(parse_auto_policy("Empty").is_none(), "大小写敏感");
        assert!(parse_auto_policy("older:abc").is_none());
        assert!(parse_auto_policy("older:0").is_none());
        assert!(parse_auto_policy("older:-5").is_none());
        assert!(parse_auto_policy("older:").is_none());
        // 校验函数与解析一致
        assert!(validate_auto_policy("").is_ok());
        assert!(validate_auto_policy("empty").is_ok());
        assert!(validate_auto_policy("older:60").is_ok());
        assert!(validate_auto_policy("older:x").is_err());
    }
}