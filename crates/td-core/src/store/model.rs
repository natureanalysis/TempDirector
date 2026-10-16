//! 数据模型。

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::util;

/// 重定向目标根（一块 RAM 盘，或 RAM 盘下的某个子目录）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Root {
    pub id: i64,
    pub label: String,
    /// 路径模板，例如 `R:` 或 `R:\Redirect`。
    pub path: String,
    pub is_ramdisk: bool,
    /// 开机等待该卷就绪的上限（毫秒）。
    pub wait_timeout_ms: i64,
    pub created_at: String,
}

impl Root {
    pub fn resolved_path(&self) -> (PathBuf, Vec<String>) {
        let (raw, unknown) = util::expand_path(&self.path);
        let normalized = util::normalize_root_path(&raw.to_string_lossy());
        (PathBuf::from(normalized), unknown)
    }

    pub fn as_str(&self) -> String {
        format!("{} [{}]", self.label, self.path)
    }
}

/// 一条重定向规则。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rule {
    pub id: i64,
    pub name: String,
    pub category: String,
    /// 源路径模板（未展开，可能含 `%APPDATA%`）。
    pub source_template: String,
    /// 归一化后的源路径，做唯一键用。
    pub source_norm: String,
    pub target_root_id: i64,
    pub target_subpath: String,
    pub link_type: String,
    /// 用户期望态："我想让它开着"。
    pub want_enabled: bool,
    pub rebuild_on_boot: bool,
    pub acl_sddl: Option<String>,
    pub needs_elevation: bool,
    pub notes: Option<String>,
    pub sort_order: i64,
    /// 清理排除模式。只影响 `clean`（命中子树不删不剪），不改搬运语义。
    pub excludes: Vec<String>,

    pub last_state: Option<String>,
    pub last_checked_at: Option<String>,
    pub last_error: Option<String>,
    pub last_files: Option<i64>,
    pub last_bytes: Option<i64>,
    pub applied_at: Option<String>,

    pub created_at: String,
    pub updated_at: String,
}

impl Rule {
    /// 按当前用户上下文展开的源路径（外加未能解析的变量名）。
    pub fn resolved_source(&self) -> (PathBuf, Vec<String>) {
        util::expand_path(&self.source_template)
    }

    /// 目标完整路径 = 目标根 + 相对子路径。
    pub fn resolved_target(&self, root: &Root) -> (PathBuf, Vec<String>) {
        let (base, unknown) = root.resolved_path();
        let sub = self.target_subpath.trim().trim_start_matches(['\\', '/']);
        if sub.is_empty() {
            (base, unknown)
        } else {
            (base.join(sub), unknown)
        }
    }

    pub fn category_label(&self) -> &'static str {
        category_label(&self.category)
    }

    /// 给 UI 用的一行摘要。
    pub fn summary(&self) -> String {
        format!(
            "#{} [{}] {} → {}",
            self.id,
            self.category_label(),
            self.source_template,
            self.target_subpath
        )
    }
}

pub fn category_label(cat: &str) -> &'static str {
    match cat {
        "browser" => "浏览器",
        "design" => "设计",
        "dev" => "开发",
        "system" => "系统",
        "media" => "影音",
        "office" => "办公",
        "game" => "游戏",
        _ => "其他",
    }
}

pub const CATEGORIES: &[&str] = &[
    "browser", "design", "dev", "system", "media", "office", "game", "other",
];

/// 新建规则的入参。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NewRule {
    pub name: String,
    pub category: String,
    pub source_template: String,
    pub target_root_id: i64,
    pub target_subpath: String,
    pub link_type: String,
    pub want_enabled: bool,
    pub rebuild_on_boot: bool,
    pub notes: Option<String>,
    #[serde(default)]
    pub excludes: Vec<String>,
}

/// 操作流水的一行。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Operation {
    pub id: i64,
    pub rule_id: Option<i64>,
    pub kind: String,
    pub phase: String,
    pub source_path: Option<String>,
    pub target_path: Option<String>,
    pub file_count: Option<i64>,
    pub bytes: Option<i64>,
    pub error: Option<String>,
    pub started_at: String,
    pub finished_at: Option<String>,
}

/// 事件日志的一行。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventRow {
    pub id: i64,
    pub rule_id: Option<i64>,
    pub level: String,
    pub kind: String,
    pub message: String,
    pub at: String,
}

pub const RULE_COLUMNS: &str = "id, name, category, source_template, source_norm, \
     target_root_id, target_subpath, link_type, want_enabled, rebuild_on_boot, \
     acl_sddl, needs_elevation, notes, sort_order, excludes, \
     last_state, last_checked_at, last_error, last_files, last_bytes, applied_at, \
     created_at, updated_at";

/// DB 里的 `excludes` 列（JSON 数组文本）→ `Vec<String>`。
/// 手工改库写坏 JSON 时按"无排除"处理，不让一条脏数据拖垮整个列表。
fn excludes_from_db(text: String) -> Vec<String> {
    serde_json::from_str(&text).unwrap_or_default()
}

pub fn rule_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Rule> {
    Ok(Rule {
        id: row.get(0)?,
        name: row.get(1)?,
        category: row.get(2)?,
        source_template: row.get(3)?,
        source_norm: row.get(4)?,
        target_root_id: row.get(5)?,
        target_subpath: row.get(6)?,
        link_type: row.get(7)?,
        want_enabled: row.get(8)?,
        rebuild_on_boot: row.get(9)?,
        acl_sddl: row.get(10)?,
        needs_elevation: row.get(11)?,
        notes: row.get(12)?,
        sort_order: row.get(13)?,
        excludes: excludes_from_db(row.get(14)?),
        last_state: row.get(15)?,
        last_checked_at: row.get(16)?,
        last_error: row.get(17)?,
        last_files: row.get(18)?,
        last_bytes: row.get(19)?,
        applied_at: row.get(20)?,
        created_at: row.get(21)?,
        updated_at: row.get(22)?,
    })
}

pub const ROOT_COLUMNS: &str = "id, label, path, is_ramdisk, wait_timeout_ms, created_at";

pub fn root_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Root> {
    Ok(Root {
        id: row.get(0)?,
        label: row.get(1)?,
        path: row.get(2)?,
        is_ramdisk: row.get(3)?,
        wait_timeout_ms: row.get(4)?,
        created_at: row.get(5)?,
    })
}
