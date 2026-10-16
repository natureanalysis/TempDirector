//! 配置迁移：export / import。
//!
//! export 走 `usecase::export_config`（与 GUI 同一条路径）；
//! import 保留对 `portable` 的直接调用 —— CLI 在导入前要检查 `bundle.app`
//! 并对非本程序导出的文件打印警告，这个细节在 usecase 门面之外。

use td_core::portable;
use td_core::store::Store;
use td_core::usecase;
use td_core::{TdError, TdResult};

use crate::parse::Parsed;

pub(crate) fn cmd_export(store: &Store, p: &Parsed) -> TdResult<()> {
    let path = p.required(0, "<输出文件.json>")?;
    let (roots, rules) = usecase::export_config(store, std::path::Path::new(&path))?;
    println!("已导出 {roots} 个目标根、{rules} 条规则 → {path}");
    println!("提示：跨机器分享时可把目标根路径改成 {{RAM_ROOT}}，导入时用 --ram-root 指定");
    Ok(())
}

pub(crate) fn cmd_import(store: &Store, p: &Parsed) -> TdResult<()> {
    let path = p.required(0, "<输入文件.json>")?;
    let text = std::fs::read_to_string(&path)
        .map_err(|e| TdError::Other(format!("读取 {path} 失败：{e}")))?;
    let bundle: portable::Bundle = serde_json::from_str(&text)
        .map_err(|e| TdError::Other(format!("解析 {path} 失败：{e}")))?;

    if bundle.app != portable::APP_TAG {
        println!(
            "警告：这份配置来自 {}，不是 TempDirector 导出的，将尽力导入",
            bundle.app
        );
    }
    let report = portable::import_bundle(store, &bundle, p.value("ram-root"), p.has("dry-run"))?;
    println!("{}", report.one_line());
    for e in &report.errors {
        println!("  ⚠ {e}");
    }
    Ok(())
}