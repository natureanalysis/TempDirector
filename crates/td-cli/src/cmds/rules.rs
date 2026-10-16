//! `rules` 子命令：规则的增删查与期望开关。

use td_core::store::{NewRule, Store, CATEGORIES};
use td_core::{TdError, TdResult};

use crate::output::{print_rule_row, yn};
use crate::parse::Parsed;

pub(crate) fn cmd_rules(store: &Store, p: &Parsed) -> TdResult<()> {
    match p.pos(0).unwrap_or("list") {
        "list" => {
            let category = p.value("category");
            let search = p.value("search");
            let enabled_only = p.has("enabled");
            let rules = store.query_rules(category, enabled_only, search)?;
            if p.json() {
                println!("{}", serde_json::to_string_pretty(&rules)?);
                return Ok(());
            }
            if rules.is_empty() {
                println!("（无匹配规则）");
                return Ok(());
            }
            println!(
                "共 {} 条（分类：{}）",
                rules.len(),
                category.unwrap_or("全部")
            );
            println!();
            for (i, r) in rules.iter().enumerate() {
                print_rule_row(r);
                if i + 1 < rules.len() {
                    println!();
                }
            }
            Ok(())
        }
        "add" => {
            let name = p.required(1, "<名称>")?;
            let source = p.required(2, "<源路径模板，如 %LOCALAPPDATA%\\X\\Cache>")?;
            let subpath = p.required(3, "<目标子目录，如 browsers\\chrome>")?;

            let root_id = match p.value("root") {
                Some(v) => v
                    .parse::<i64>()
                    .map_err(|_| TdError::Other("--root 必须是整数 id".into()))?,
                None => {
                    let roots = store.list_roots()?;
                    match roots.len() {
                        0 => {
                            return Err(TdError::Other(
                                "还没有配置目标根，先执行：roots add RAMDisk R:".into(),
                            ))
                        }
                        1 => roots[0].id,
                        _ => {
                            let list = roots
                                .iter()
                                .map(|r| format!("#{} {}", r.id, r.label))
                                .collect::<Vec<_>>()
                                .join("、");
                            return Err(TdError::Other(format!(
                                "有多个目标根，请用 --root 指定：{list}"
                            )));
                        }
                    }
                }
            };

            let category = p.value("category").unwrap_or("other").to_string();
            if !CATEGORIES.contains(&category.as_str()) {
                return Err(TdError::Other(format!(
                    "未知分类 {category}，可用：{}",
                    CATEGORIES.join(" / ")
                )));
            }
            let link_type = p.value("link-type").unwrap_or("junction").to_string();
            if td_core::link::LinkType::parse(&link_type).is_none() {
                return Err(TdError::Other(
                    "--link-type 只能是 junction 或 symlink_dir".into(),
                ));
            }

            // CLI 直连 store，不经过 usecase 的表单校验，这里复用同一套
            // 排除模式校验，防止垃圾模式静默入库（清理时被容错为空而失去保护）。
            let excludes: Vec<String> = p.values("exclude").to_vec();
            td_core::fsops::exclude::Excludes::new(&excludes)?;

            let nr = NewRule {
                name,
                category,
                source_template: source,
                target_root_id: root_id,
                target_subpath: subpath,
                link_type,
                want_enabled: p.has("enable"),
                rebuild_on_boot: !p.has("no-boot"),
                notes: p.value("notes").map(|s| s.to_string()),
                excludes,
            };
            let id = store.insert_rule(&nr)?;
            let rule = store.get_rule(id)?;
            println!("已添加规则 #{id}");
            print_rule_row(&rule);
            if rule.needs_elevation {
                println!("提示：该源路径位于受保护位置，应用时需要管理员权限");
            }
            Ok(())
        }
        "rm" | "remove" | "delete" => {
            for id in p.ids(1)? {
                store.delete_rule(id)?;
                println!("已删除规则 #{id}");
            }
            Ok(())
        }
        "enable" | "disable" => {
            let on = p.pos(0) == Some("enable");
            let ids = if p.has("all") {
                store.list_rules()?.into_iter().map(|r| r.id).collect()
            } else {
                p.ids(1)?
            };
            if ids.is_empty() {
                return Err(TdError::Other("请指定规则 id 或 --all".into()));
            }
            for id in ids {
                store.set_rule_enabled(id, on)?;
                println!("规则 #{id} 期望开关 → {}", yn(on));
            }
            Ok(())
        }
        other => Err(TdError::Other(format!(
            "未知的 rules 子命令：{other}（可用：list / add / rm / enable / disable）"
        ))),
    }
}