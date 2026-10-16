//! 清理排除模式（fnmatch 风格，gitignore 的一个可解释子集）。
//!
//! ## 语义
//!
//! 模式针对**相对清理根目录**的路径（`\` 与 `/` 等价）：
//!
//! * 纯名字模式（不含分隔符，如 `sessions`、`*.log`）——匹配**任意层级**：
//!   路径中任何一段命中即排除（目录命中则整棵子树不进入）。
//! * 含分隔符模式（如 `logs/old`、`*/cache`）——**从根锚定**：
//!   整条相对路径与模式做 fnmatch。目录命中同样剪掉整棵子树
//!   （`logs/old` 能保护 `logs/old/` 下的一切）。
//! * `*` 匹配单层内任意字符（不跨分隔符），`**` 单独成段时跨层，
//!   `?` 匹配单个字符。
//!
//! ## 只影响清理
//!
//! 排除列表**不改变 apply/restore 的搬运语义**：搬运永远全量
//! （建链要求源路径腾空）。它的职责是让 `clean` 不误删缓存目录里
//! 混着的"不能丢"的东西。想让重要数据不进 RAM 盘，应把规则的源路径
//! 收窄到纯缓存子目录，而不是依赖排除。

use crate::error::{TdError, TdResult};
use crate::i18n::{t, tf};

/// 单条模式的最大长度与单规则的模式数上限（防表单塞垃圾）。
const MAX_PATTERN_LEN: usize = 120;
const MAX_PATTERNS: usize = 16;

#[derive(Debug, Clone, Default)]
pub struct Excludes {
    /// 含分隔符的模式（按 `/` 预拆段）。
    anchored: Vec<Vec<String>>,
    /// 纯名字模式（任意层级生效）。
    names: Vec<String>,
}

impl Excludes {
    pub fn new(patterns: &[String]) -> TdResult<Self> {
        if patterns.len() > MAX_PATTERNS {
            return Err(TdError::InvalidPath(tf("exclude.too_many", &[
                ("max", &MAX_PATTERNS.to_string()),
            ])));
        }
        let mut ex = Excludes::default();
        for p in patterns {
            let norm = p.trim().replace('\\', "/");
            if norm.is_empty() {
                return Err(TdError::InvalidPath(t("exclude.empty").into()));
            }
            if norm.len() > MAX_PATTERN_LEN {
                return Err(TdError::InvalidPath(tf("exclude.too_long", &[
                    ("max", &MAX_PATTERN_LEN.to_string()),
                    ("pat", p),
                ])));
            }
            let segs: Vec<&str> = norm.split('/').collect();
            if segs.contains(&"..") {
                return Err(TdError::InvalidPath(tf("exclude.no_parent", &[
                    ("pat", p),
                ])));
            }
            if segs.len() == 1 {
                ex.names.push(norm);
            } else {
                ex.anchored
                    .push(segs.into_iter().map(|s| s.to_string()).collect());
            }
        }
        Ok(ex)
    }

    pub fn is_empty(&self) -> bool {
        self.anchored.is_empty() && self.names.is_empty()
    }

    /// 相对路径（以 `/` 分隔）是否命中任一排除模式。
    pub fn matches(&self, rel: &str) -> bool {
        if self.is_empty() {
            return false;
        }
        let segs: Vec<&str> = rel.split(['/', '\\']).filter(|s| !s.is_empty()).collect();
        // 纯名字模式：任意一段命中即排除（含最后一段 = 文件名）。
        for pat in &self.names {
            if segs.iter().any(|s| seg_match(pat, s)) {
                return true;
            }
        }
        // 锚定模式：整条相对路径匹配。
        for pat in &self.anchored {
            if segs_match(pat, &segs) {
                return true;
            }
        }
        false
    }
}

/// 段序列匹配：`**` 单独成段时跨层，其余段逐段 [`seg_match`]。
/// 模式耗尽而路径还有剩余段 → 视为**前缀命中**（目录模式保护整棵子树）。
fn segs_match(pat: &[String], segs: &[&str]) -> bool {
    match (pat.first(), segs.first()) {
        (None, _) => true,
        (Some(_), None) => false,
        (Some(p), Some(s)) => {
            if p == "**" {
                // `**` 吃掉 0..n 段。
                (0..=segs.len()).any(|skip| segs_match(&pat[1..], &segs[skip..]))
            } else {
                seg_match(p, s) && segs_match(&pat[1..], &segs[1..])
            }
        }
    }
}

/// 单段内 glob：`*` 任意长度、`?` 单字符，不含跨层语义。
fn seg_match(pat: &str, s: &str) -> bool {
    let p: Vec<char> = pat.chars().collect();
    let t: Vec<char> = s.chars().collect();
    // 经典双指针 + 回溯处理 `*`。
    let (mut pi, mut ti) = (0usize, 0usize);
    let (mut star, mut mark) = (usize::MAX, 0usize);
    while ti < t.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == t[ti]) {
            pi += 1;
            ti += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = pi;
            mark = ti;
            pi += 1;
        } else if star != usize::MAX {
            pi = star + 1;
            mark += 1;
            ti = mark;
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == '*' {
        pi += 1;
    }
    pi == p.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ex(pats: &[&str]) -> Excludes {
        let v: Vec<String> = pats.iter().map(|s| s.to_string()).collect();
        Excludes::new(&v).unwrap()
    }

    #[test]
    fn plain_name_matches_any_level() {
        let e = ex(&["sessions"]);
        assert!(e.matches("sessions"));
        assert!(e.matches("profiles/x/sessions"));
        assert!(e.matches("sessions/db.sqlite"));
        assert!(!e.matches("session"));
        assert!(!e.matches("a/sessions2/b"));
    }

    #[test]
    fn star_does_not_cross_separator() {
        let e = ex(&["*.log"]);
        assert!(e.matches("a.log"));
        assert!(e.matches("sub/b.log"));
        assert!(!e.matches("a/logs")); // `*` 不跨层
    }

    #[test]
    fn anchored_pattern_matches_from_root_only() {
        let e = ex(&["logs/old"]);
        assert!(e.matches("logs/old"));
        assert!(e.matches("logs/old/x.txt"));
        assert!(!e.matches("deep/logs/old"), "锚定模式不从任意层级起算");
        assert!(!e.matches("logs"));
    }

    #[test]
    fn double_star_crosses_levels() {
        let e = ex(&["logs/**/old"]);
        assert!(e.matches("logs/old"));
        assert!(e.matches("logs/a/b/old"));
        assert!(e.matches("logs/a/b/old/x"));
        assert!(!e.matches("a/logs/old"), "首段仍是锚定的");
    }

    #[test]
    fn question_mark_single_char() {
        let e = ex(&["v?"]);
        assert!(e.matches("v1"));
        assert!(e.matches("x/v9"));
        assert!(!e.matches("v10"));
    }

    #[test]
    fn backslash_is_normalized() {
        let e = ex(&["logs\\old"]);
        assert!(e.matches("logs/old"), "Windows 风格模式与 Unix 风格路径等价");
        assert!(e.matches("logs\\old"), "反斜杠路径同样命中");
    }

    #[test]
    fn empty_excludes_match_nothing() {
        let e = Excludes::default();
        assert!(!e.matches("anything/at/all"));
        assert!(e.is_empty());
    }

    #[test]
    fn validation_rejects_garbage() {
        let mk = |pats: &[&str]| {
            let v: Vec<String> = pats.iter().map(|s| s.to_string()).collect();
            Excludes::new(&v)
        };
        assert!(mk(&[""]).is_err(), "空模式");
        assert!(mk(&["a/.."]).is_err(), ".. 段");
        assert!(mk(&["ok", ""]).is_err(), "混合里的空模式");
        let many: Vec<String> = (0..17).map(|i| format!("p{i}")).collect();
        assert!(Excludes::new(&many).is_err(), "超过 16 条");
        let long = "x".repeat(MAX_PATTERN_LEN + 1);
        assert!(mk(&[&long]).is_err(), "超长模式");
        assert!(mk(&["ok"]).is_ok());
    }

    #[test]
    fn seg_match_basics() {
        assert!(seg_match("*", ""));
        assert!(seg_match("a*c", "abc"));
        assert!(seg_match("a*c", "ac"));
        assert!(!seg_match("a*c", "abd"));
        assert!(seg_match("a?c", "a+c"));
        assert!(!seg_match("a?c", "ac"));
        assert!(seg_match("平台无关", "平台无关"), "非 ASCII 同样匹配");
    }
}