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