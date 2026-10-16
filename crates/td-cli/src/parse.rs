//! 极简参数解析（不引入 clap：本程序的参数面很窄，手写更可控也更少依赖）。
//!
//! 约定：`--flag`、`--key value`、`--key=value` 与位置参数；
//! 值以下一个 `--` 开头时视为未提供（该 flag 无值）。

use std::collections::HashMap;

use td_core::{TdError, TdResult};

#[derive(Debug, Default)]
pub(crate) struct Parsed {
    positional: Vec<String>,
    flags: HashMap<String, Option<String>>,
}

impl Parsed {
    pub(crate) fn parse(args: &[String]) -> Self {
        let mut out = Parsed::default();
        let mut i = 0usize;
        while i < args.len() {
            let a = &args[i];
            if let Some(rest) = a.strip_prefix("--") {
                if let Some((k, v)) = rest.split_once('=') {
                    out.flags.insert(k.to_string(), Some(v.to_string()));
                } else {
                    let next_is_value = args
                        .get(i + 1)
                        .map(|n| !n.starts_with("--"))
                        .unwrap_or(false);
                    if next_is_value {
                        out.flags
                            .insert(rest.to_string(), Some(args[i + 1].clone()));
                        i += 1;
                    } else {
                        out.flags.insert(rest.to_string(), None);
                    }
                }
            } else {
                out.positional.push(a.clone());
            }
            i += 1;
        }
        out
    }

    pub(crate) fn has(&self, name: &str) -> bool {
        self.flags.contains_key(name)
    }

    pub(crate) fn value(&self, name: &str) -> Option<&str> {
        self.flags.get(name).and_then(|v| v.as_deref())
    }

    /// 位置参数（可被 `--` 后的内容追加）。
    pub(crate) fn pos(&self, idx: usize) -> Option<&str> {
        self.positional.get(idx).map(|s| s.as_str())
    }

    pub(crate) fn required(&self, idx: usize, what: &str) -> TdResult<String> {
        self.pos(idx)
            .map(|s| s.to_string())
            .ok_or_else(|| TdError::Other(format!("缺少参数：{what}")))
    }

    /// 解析 id 列表（`rules enable 1 2 3`）或 `--all`。
    pub(crate) fn ids(&self, start: usize) -> TdResult<Vec<i64>> {
        let mut out = Vec::new();
        let mut i = start;
        while let Some(s) = self.pos(i) {
            out.push(
                s.parse::<i64>()
                    .map_err(|_| TdError::Other(format!("不是合法的规则 id：{s}")))?,
            );
            i += 1;
        }
        Ok(out)
    }

    pub(crate) fn json(&self) -> bool {
        self.has("json")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parsed(args: &[&str]) -> Parsed {
        Parsed::parse(&args.iter().map(|s| s.to_string()).collect::<Vec<_>>())
    }

    #[test]
    fn flags_and_positionals() {
        // 注意语义：flag 后跟非 `--` 开头的参数会被当成它的值
        // （`--json target` 里 target 是 --json 的值），所以位置参数要放在 flag 前面。
        let p = parsed(&["target", "extra", "--empty", "--older-than", "1h", "--json"]);
        assert!(p.has("empty"));
        assert!(p.has("json"));
        assert_eq!(p.value("older-than"), Some("1h"));
        assert_eq!(p.pos(0), Some("target"));
        assert_eq!(p.pos(1), Some("extra"));
        assert!(p.ids(0).is_err(), "位置参数不是数字时应报错");
    }

    #[test]
    fn ids_parses_integers_from_start() {
        let p = parsed(&["enable", "1", "2", "3"]);
        assert_eq!(p.ids(1).unwrap(), vec![1, 2, 3]);
        assert!(parsed(&["enable", "x"]).ids(1).is_err());
    }

    #[test]
    fn equals_form_and_bare_flag_without_value() {
        let p = parsed(&["--limit=50", "--dry-run", "--limit2"]);
        assert_eq!(p.value("limit"), Some("50"));
        assert!(p.value("dry-run").is_none(), "后随 flag 时不应吃掉它当值");
        assert!(p.has("dry-run"));
        assert!(p.has("limit2"));
    }

    #[test]
    fn required_reports_missing_param() {
        let p = parsed(&["only"]);
        assert!(p.required(1, "<路径>").is_err());
    }
}