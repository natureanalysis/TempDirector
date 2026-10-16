-- TempDirector 规则库结构 v1
--
-- 设计要点：
-- * `want_enabled`（用户期望态）与 `last_state`（上次检测到的真实状态）分开存。
--   UI 勾选框改的是前者，状态图标显示的是后者 —— "勾了但没生效"必须能被表达出来。
-- * `source_template` 存**未展开**的路径模板（含 %APPDATA% 等），运行时按当前用户展开，
--   这样才能一份规则库服务多用户 / 多机器。
-- * `acl_sddl` 只在需要时才非空：给重建出来的空目录回灌原本的 DACL，
--   避免"以 SYSTEM 身份重建的目录权限不对，应用写不进去"。
-- * 崩溃可恢复：非终态的 `operations` 行就是待修复清单，不给 rules 加布尔标记。

CREATE TABLE IF NOT EXISTS roots (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    label           TEXT    NOT NULL,
    path            TEXT    NOT NULL UNIQUE,
    is_ramdisk      INTEGER NOT NULL DEFAULT 1,
    wait_timeout_ms INTEGER NOT NULL DEFAULT 90000,
    created_at      TEXT    NOT NULL DEFAULT (datetime('now'))
);

CREATE TABLE IF NOT EXISTS rules (
    id                INTEGER PRIMARY KEY AUTOINCREMENT,
    name              TEXT    NOT NULL,
    category          TEXT    NOT NULL DEFAULT 'other',
    source_template   TEXT    NOT NULL,
    source_norm       TEXT    NOT NULL UNIQUE,
    target_root_id    INTEGER NOT NULL REFERENCES roots(id) ON DELETE RESTRICT,
    target_subpath    TEXT    NOT NULL,
    link_type         TEXT    NOT NULL DEFAULT 'junction'
                              CHECK (link_type IN ('junction', 'symlink_dir')),
    want_enabled      INTEGER NOT NULL DEFAULT 0,
    rebuild_on_boot   INTEGER NOT NULL DEFAULT 1,
    acl_sddl          TEXT,
    needs_elevation   INTEGER NOT NULL DEFAULT 0,
    notes             TEXT,
    sort_order        INTEGER NOT NULL DEFAULT 0,
    -- 清理排除模式（JSON 数组，如 ["sessions","logs/old"]）。
    -- 只影响 clean：命中的子树不删不剪；apply/restore 搬运永远是全量。
    excludes          TEXT NOT NULL DEFAULT '[]',
    -- 水位预授权：空 = 未启用；empty = 水位 critical 时清空（仍受 excludes 保护）；
    -- older:<秒> = 只清该秒之前修改的。仅 water 巡检触发，用户手动 clean 不受影响。
    auto_clean_policy TEXT NOT NULL DEFAULT '',

    -- 以下为状态缓存，真相由实时检测给出（last_* 只用于列表排序与离线展示）
    last_state        TEXT,
    last_checked_at   TEXT,
    last_error        TEXT,
    last_files        INTEGER,
    last_bytes        INTEGER,
    applied_at        TEXT,

    created_at        TEXT    NOT NULL DEFAULT (datetime('now')),
    updated_at        TEXT    NOT NULL DEFAULT (datetime('now'))
);

CREATE INDEX IF NOT EXISTS idx_rules_cat     ON rules(category, sort_order);
CREATE INDEX IF NOT EXISTS idx_rules_enabled ON rules(want_enabled, rebuild_on_boot);
CREATE INDEX IF NOT EXISTS idx_rules_root    ON rules(target_root_id);

CREATE TABLE IF NOT EXISTS operations (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    rule_id     INTEGER REFERENCES rules(id) ON DELETE SET NULL,
    kind        TEXT NOT NULL,
    phase       TEXT NOT NULL,
    source_path TEXT,
    target_path TEXT,
    file_count  INTEGER,
    bytes       INTEGER,
    error       TEXT,
    started_at  TEXT NOT NULL DEFAULT (datetime('now')),
    finished_at TEXT
);

CREATE INDEX IF NOT EXISTS idx_ops_open ON operations(phase);

CREATE TABLE IF NOT EXISTS events (
    id      INTEGER PRIMARY KEY AUTOINCREMENT,
    rule_id INTEGER REFERENCES rules(id) ON DELETE CASCADE,
    level   TEXT NOT NULL,
    kind    TEXT NOT NULL,
    message TEXT NOT NULL,
    at      TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE INDEX IF NOT EXISTS idx_events_at ON events(at DESC);

CREATE TABLE IF NOT EXISTS settings (
    k TEXT PRIMARY KEY,
    v TEXT NOT NULL
);
