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

use crate::error::TdResult;
use crate::fsops::mover::volume_space;
use crate::store::Store;
use crate::util;

use super::view::WaterLevel;

/// 默认告警线（可用 settings 键 `water_warn_pct` 覆盖）。
pub const DEFAULT_WARN_PCT: f64 = 85.0;
/// 默认严重线（可用 settings 键 `water_critical_pct` 覆盖）。
pub const DEFAULT_CRIT_PCT: f64 = 95.0;

/// settings 里记「上次各根水位级别」的键（JSON：`{路径: 级别}`）。
const STATE_KEY: &str = "water_level_state";

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
                    let msg = format!("已用 {pct:.1}%，剩余 {}", util::human_bytes(free_b));
                    (lv.to_string(), pct, free_b, util::human_bytes(free_b), util::human_bytes(total), msg)
                }
                Ok((total, _)) => (
                    "unknown".into(),
                    0.0,
                    0,
                    util::human_bytes(0),
                    util::human_bytes(total),
                    "卷容量为 0，无法判断水位".into(),
                ),
                Err(_) => (
                    "unknown".into(),
                    0.0,
                    0,
                    util::human_bytes(0),
                    util::human_bytes(0),
                    "卷不可用（未挂载？），无法读取水位".into(),
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
        let Some(from) = old.get(&lv.path) else { continue };
        if from == &lv.level {
            continue;
        }
        let (ev_level, msg) = match lv.level.as_str() {
            "critical" => (
                "error",
                format!(
                    "RAM 盘即将写满：{}（{}）已用 {:.1}%，仅剩 {}，写满会导致软件写缓存失败",
                    lv.label, lv.path, lv.used_pct, lv.free_text
                ),
            ),
            "warn" => (
                "warn",
                format!(
                    "RAM 盘水位偏高：{}（{}）已用 {:.1}%，剩余 {}",
                    lv.label, lv.path, lv.used_pct, lv.free_text
                ),
            ),
            _ => (
                "info",
                format!(
                    "水位恢复：{}（{}）已用 {:.1}%，剩余 {}",
                    lv.label, lv.path, lv.used_pct, lv.free_text
                ),
            ),
        };
        store.log_event(None, ev_level, "water", &msg)?;
    }

    if old != new_state {
        store.set_setting(STATE_KEY, &serde_json::to_string(&new_state)?)?;
    }
    Ok(levels)
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
}