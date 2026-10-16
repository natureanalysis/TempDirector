//! `roots` 子命令：目标根（RAM 盘）的增删查。

use td_core::boot::probe_volume;
use td_core::store::Store;
use td_core::{TdError, TdResult};

use crate::parse::Parsed;

pub(crate) fn cmd_roots(store: &Store, p: &Parsed) -> TdResult<()> {
    match p.pos(0).unwrap_or("list") {
        "list" => {
            let roots = store.list_roots()?;
            if roots.is_empty() {
                println!("（未配置，示例：roots add RAMDisk R: ）");
            }
            for r in roots {
                let (path, unknown) = r.resolved_path();
                let (state, msg) = probe_volume(&path);
                println!(
                    "#{:<3} {:<20} {:<20} 等待上限 {}ms  盘：{}  {}\n     {}",
                    r.id,
                    r.label,
                    r.path,
                    r.wait_timeout_ms,
                    if r.is_ramdisk { "RAM" } else { "普通" },
                    state,
                    msg
                );
                if !unknown.is_empty() {
                    println!("     未解析变量：{}", unknown.join("、"));
                }
            }
            Ok(())
        }
        "add" => {
            let label = p.required(1, "<label>")?;
            let path = p.required(2, "<path>")?;
            let is_ram = !p.has("hdd");
            let timeout = p
                .value("timeout")
                .map(|v| v.parse::<i64>())
                .transpose()
                .map_err(|_| TdError::Other("--timeout 必须是整数毫秒".into()))?
                .unwrap_or(90000);
            let id = store.insert_root(&label, &path, is_ram, timeout)?;
            println!("已添加目标根 #{id}: {label} → {path}");
            Ok(())
        }
        "rm" | "remove" | "delete" => {
            let id = p.required(1, "<id>")?.parse::<i64>().map_err(|_| {
                TdError::Other("目标根 id 必须是整数".into())
            })?;
            store.delete_root(id)?;
            println!("已删除目标根 #{id}");
            Ok(())
        }
        other => Err(TdError::Other(format!(
            "未知的 roots 子命令：{other}（可用：list / add / rm）"
        ))),
    }
}