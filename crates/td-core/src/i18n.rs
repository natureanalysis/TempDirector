//! 运行时多语言文案（中 / 英）。
//!
//! 设计原则：**文案在产生它的那一层按当前语言解析**，而不是把某一语言的原文
//! 透传到上层再翻译。调用方只持有稳定的键（如 `"err.io"`），当前语言由进程内
//! 全局状态 [`set_locale`] 决定：
//!
//! * GUI：前端切换语言后经 `set_locale` 命令写回这里，随后重新拉取数据；
//! * CLI：`--lang` / `TEMPDIRECTOR_LANG` / 系统区域设置在启动时写入。
//!
//! 词典用 `match` 实现：零依赖、编译器可查缺键、无需任何初始化。
//! 文案里的占位符写作 `{name}`，由 [`tf`] 负责插值。

use std::sync::atomic::{AtomicU8, Ordering};

/// 支持的界面语言。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Locale {
    Zh,
    En,
}

impl Locale {
    /// 稳定的语言标签（写配置 / 传前端用）。
    pub const fn tag(self) -> &'static str {
        match self {
            Locale::Zh => "zh",
            Locale::En => "en",
        }
    }

    /// 从标签 / 系统区域串解析；无法识别返回 `None`。
    pub fn parse(s: &str) -> Option<Self> {
        let s = s.trim().to_ascii_lowercase();
        if s.starts_with("zh") {
            Some(Locale::Zh)
        } else if s.starts_with("en") {
            Some(Locale::En)
        } else {
            None
        }
    }
}

impl Default for Locale {
    fn default() -> Self {
        Locale::Zh
    }
}

const ZH: u8 = 0;
const EN: u8 = 1;

static CURRENT: AtomicU8 = AtomicU8::new(ZH);

/// 设置当前语言。
pub fn set_locale(locale: Locale) {
    CURRENT.store(
        match locale {
            Locale::Zh => ZH,
            Locale::En => EN,
        },
        Ordering::Relaxed,
    );
}

/// 读取当前语言。
pub fn locale() -> Locale {
    if CURRENT.load(Ordering::Relaxed) == EN {
        Locale::En
    } else {
        Locale::Zh
    }
}

/// 查一条静态文案，返回当前语言版本。未收录的键返回空串。
pub fn t(key: &str) -> &'static str {
    match locale() {
        Locale::Zh => zh(key),
        Locale::En => en(key),
    }
}

/// 查一条带占位符的文案并插值。占位符写作 `{name}`，`args` 给出 `("name", 值)`。
pub fn tf(key: &str, args: &[(&str, &str)]) -> String {
    let mut out = t(key).to_string();
    for (k, v) in args {
        out = out.replace(&format!("{{{k}}}"), v);
    }
    out
}

fn zh(key: &str) -> &'static str {
    match key {
        "err.io" => "IO 错误：{e}",
        "err.db" => "数据库错误：{e}",
        "err.json" => "JSON 处理错误：{e}",
        "err.invalid_path" => "路径非法：{e}",
        "err.safety" => "安全检查未通过：{e}",
        "err.busy" => "被占用：{e}",
        "err.no_space" => "目标卷空间不足：需要 {need}，可用 {free}",
        "err.verify" => "数据核对失败：{e}",
        "err.elevation" => "需要管理员权限：{e}",
        "err.not_found" => "找不到记录：{e}",
        "io.failed" => "{action}失败：{err}{hint}（路径：{path}）",
        "hint.busy" => "（文件正被其他程序占用，请先关闭相关软件）",
        "hint.denied" => "（访问被拒绝，该路径需要管理员权限）",
        "hint.not_exist" => "（路径不存在）",
        "hint.no_space" => "（磁盘空间不足）",
        "hint.too_long" => "（路径过长，已启用 \\\\?\\ 前缀仍失败请缩短目录层级）",
        "hint.not_reparse" => "（不是重解析点）",
        "hint.none" => "",
        "link.type.junction" => "目录联接",
        "link.type.symlink" => "目录符号链接",
        "link.state.source_missing" => "源路径不存在",
        "link.state.plain_dir" => "未链接（真实目录）",
        "link.state.plain_file" => "路径是文件，规则配置有误",
        "link.state.target_missing" => "目标丢失",
        "link.state.target_empty" => "目标为空（RAM 盘刚重启）",
        "link.state.target_ok" => "正常",
        "link.state.mismatch" => "，且指向与登记不符",
        "link.state.link_ok" => "{type} → {target}（{state}{extra}）",
        "link.state.link_broken" => "{type} 断裂，目标不可达：{target}",
        "link.state.foreign" => "外来重解析点（tag {tag}），非本工具管理",
        "link.state.access_denied" => "访问被拒绝（需要管理员权限）",
        "link.state.io_error" => "检测出错：{message}",
        "verify.mode.count" => "文件数 + 总字节数",
        "verify.mode.sha256" => "逐文件 SHA-256",
        "inspect.state.link_ok_empty" => "链接已就绪，目标为空（开机重建后的正常形态）",
        "inspect.state.link_ok" => "链接正常",
        "banner.boot_problem" => "上次开机重建（{at}）有需要留意的地方：{summary}",
        "banner.no_roots" => "还没有配置目标根（RAM 盘），先在「设置」里添加一个，例如 R:",
        "banner.all_roots_down" => "所有目标根都不可用 —— RAM 盘可能没有创建或没有挂载，此时无法建立链接",
        "banner.needs_attention" => "有 {n} 条规则需要处理（断链 / 真实目录 / 外来重解析点）",
        "banner.open_ops" => "有 {n} 条上次中断的操作未收尾，请查看「运维」面板确认后处理",
        "banner.root_error" => "目标根 #{id} {path} 异常：{message}",
        "banner.disk_critical" => "RAM 盘即将写满：{label}（{path}）已用 {pct}%，仅剩 {free} —— 建议清理或缩小规则",
        "banner.disk_warn" => "RAM 盘水位偏高：{label}（{path}）已用 {pct}%，剩余 {free}",
        "cat.browser" => "浏览器",
        "cat.design" => "设计",
        "cat.dev" => "开发",
        "cat.system" => "系统",
        "cat.media" => "影音",
        "cat.office" => "办公",
        "cat.game" => "游戏",
        "cat.other" => "其他",
        "move.method.rename" => "同卷重命名",
        "move.method.copy" => "跨卷复制",
        "autostart.unregistered" => "未注册（登录时不会自动重建）",
        "autostart.registered" => "已注册",
        "autostart.registered_cmd" => "已注册：{cmd}",
        "autostart.disabled" => "已取消开机自启",
        "autostart.not_registered" => "该任务本来就没有注册",
        "autostart.triggered" => "已触发一次开机重建任务",
        "water.message.ok" => "已用 {pct}%，剩余 {free}",
        "water.message.zero" => "卷容量为 0，无法判断水位",
        "water.message.unavailable" => "卷不可用（未挂载？），无法读取水位",
        "disk.role.ram" => "RAM 盘",
        "disk.role.target" => "目标盘",
        "disk.role.source" => "源盘",
        "worker.boot_rebuild" => "开机重建",
        "volume.not_created" => "卷已就绪，但目标根目录还没建出来：{path}（RAM 盘重启后的正常现象，运行一次开机重建即可）",
        "volume.missing" => "目标卷不存在：{path}",
        "volume.not_dir" => "目标路径不是目录：{path}",
        "volume.ready" => "就绪，可用空间 {free}",
        "volume.readonly" => "不可写入：{e}",
        "volume.no_space_info" => "无法查询容量：{e}",
        "volume.wait_timeout" => "等待目标卷就绪超时（已等 {secs} 秒）：{path}。RAM 盘未创建、未格式化，或创建它的程序还没启动。",
        "volume.create_failed" => "创建目标根目录失败（{path}）：{e}。若是 RAM 盘尚未挂载，请先创建 RAM 盘。",
        _ => "",
    }
}

fn en(key: &str) -> &'static str {
    match key {
        "err.io" => "I/O error: {e}",
        "err.db" => "Database error: {e}",
        "err.json" => "JSON error: {e}",
        "err.invalid_path" => "Invalid path: {e}",
        "err.safety" => "Safety check failed: {e}",
        "err.busy" => "In use: {e}",
        "err.no_space" => "Not enough space on target volume: need {need}, free {free}",
        "err.verify" => "Data verification failed: {e}",
        "err.elevation" => "Administrator privileges required: {e}",
        "err.not_found" => "Record not found: {e}",
        "io.failed" => "{action} failed: {err}{hint} (path: {path})",
        "hint.busy" => " (the file is held by another program; close it first)",
        "hint.denied" => " (access denied; this path needs administrator privileges)",
        "hint.not_exist" => " (path does not exist)",
        "hint.no_space" => " (not enough disk space)",
        "hint.too_long" => " (path too long; even with the \\\\?\\ prefix, shorten the directory depth)",
        "hint.not_reparse" => " (not a reparse point)",
        "hint.none" => "",
        "link.type.junction" => "Directory junction",
        "link.type.symlink" => "Directory symbolic link",
        "link.state.source_missing" => "Source path does not exist",
        "link.state.plain_dir" => "Not linked (real directory)",
        "link.state.plain_file" => "Path is a file; rule is misconfigured",
        "link.state.target_missing" => "Target missing",
        "link.state.target_empty" => "Target empty (RAM disk just restarted)",
        "link.state.target_ok" => "OK",
        "link.state.mismatch" => "; target differs from registered",
        "link.state.link_ok" => "{type} → {target} ({state}{extra})",
        "link.state.link_broken" => "{type} broken, target unreachable: {target}",
        "link.state.foreign" => "Foreign reparse point (tag {tag}), not managed by this tool",
        "link.state.access_denied" => "Access denied (administrator privileges required)",
        "link.state.io_error" => "Detection error: {message}",
        "verify.mode.count" => "File count + total bytes",
        "verify.mode.sha256" => "Per-file SHA-256",
        "inspect.state.link_ok_empty" => "Link ready, target empty (normal after boot rebuild)",
        "inspect.state.link_ok" => "Link OK",
        "banner.boot_problem" => "Last boot rebuild ({at}) has issues to review: {summary}",
        "banner.no_roots" => "No target root (RAM disk) configured; add one in \"Settings\", e.g. R:",
        "banner.all_roots_down" => "All target roots unavailable — RAM disk may not be created or mounted; cannot create links",
        "banner.needs_attention" => "{n} rules need attention (broken links / real directories / foreign reparse points)",
        "banner.open_ops" => "{n} interrupted operations pending; check the \"Ops\" panel to review",
        "banner.root_error" => "Target root #{id} {path} abnormal: {message}",
        "banner.disk_critical" => "RAM disk nearly full: {label} ({path}) {pct}% used, only {free} free — clean up or shrink rules",
        "banner.disk_warn" => "RAM disk water level high: {label} ({path}) {pct}% used, {free} free",
        "cat.browser" => "Browser",
        "cat.design" => "Design",
        "cat.dev" => "Development",
        "cat.system" => "System",
        "cat.media" => "Media",
        "cat.office" => "Office",
        "cat.game" => "Game",
        "cat.other" => "Other",
        "move.method.rename" => "Same-volume rename",
        "move.method.copy" => "Cross-volume copy",
        "autostart.unregistered" => "Not registered (won't auto-rebuild on login)",
        "autostart.registered" => "Registered",
        "autostart.registered_cmd" => "Registered: {cmd}",
        "autostart.disabled" => "Autostart disabled",
        "autostart.not_registered" => "Task was not registered",
        "autostart.triggered" => "Triggered a boot rebuild task",
        "water.message.ok" => "{pct}% used, {free} free",
        "water.message.zero" => "Volume capacity is 0, cannot determine water level",
        "water.message.unavailable" => "Volume unavailable (not mounted?), cannot read water level",
        "disk.role.ram" => "RAM disk",
        "disk.role.target" => "Target disk",
        "disk.role.source" => "Source disk",
        "worker.boot_rebuild" => "Boot rebuild",
        "volume.not_created" => "Volume ready but target root not yet created: {path} (normal after RAM disk restart; run boot rebuild once)",
        "volume.missing" => "Target volume does not exist: {path}",
        "volume.not_dir" => "Target path is not a directory: {path}",
        "volume.ready" => "Ready, free space {free}",
        "volume.readonly" => "Not writable: {e}",
        "volume.no_space_info" => "Cannot query capacity: {e}",
        "volume.wait_timeout" => "Timed out waiting for target volume ({secs}s): {path}. RAM disk not created, not formatted, or its creator not started.",
        "volume.create_failed" => "Failed to create target root directory ({path}): {e}. If the RAM disk is not mounted, create it first.",
        _ => "",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_recognizes_tags() {
        assert_eq!(Locale::parse("zh-CN"), Some(Locale::Zh));
        assert_eq!(Locale::parse("zh"), Some(Locale::Zh));
        assert_eq!(Locale::parse("en-US"), Some(Locale::En));
        assert_eq!(Locale::parse("EN"), Some(Locale::En));
        assert_eq!(Locale::parse("fr"), None);
    }

    #[test]
    fn switch_changes_text() {
        set_locale(Locale::Zh);
        assert_eq!(t("err.busy"), "被占用：{e}");
        set_locale(Locale::En);
        assert_eq!(t("err.busy"), "In use: {e}");
        set_locale(Locale::Zh);
    }

    #[test]
    fn interpolates_placeholders() {
        set_locale(Locale::Zh);
        let s = tf("io.failed", &[("action", "搬运"), ("err", "boom"), ("hint", ""), ("path", "C:\\x")]);
        assert_eq!(s, "搬运失败：boom（路径：C:\\x）");
        set_locale(Locale::En);
        let s = tf("io.failed", &[("action", "move"), ("err", "boom"), ("hint", ""), ("path", "C:\\x")]);
        assert_eq!(s, "move failed: boom (path: C:\\x)");
        set_locale(Locale::Zh);
    }
}