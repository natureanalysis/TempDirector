//! 开机重建与登录任务：boot-rebuild / autostart。

use std::path::PathBuf;

use td_core::usecase;
use td_core::{TdError, TdResult};

use crate::parse::Parsed;

pub(crate) fn cmd_boot_rebuild(store: &td_core::store::Store, p: &Parsed) -> TdResult<()> {
    println!("开始开机重建（模拟）…");
    if p.json() {
        let report = usecase::boot_rebuild(store)?;
        println!("{}", serde_json::to_string_pretty(&report)?);
        return Ok(());
    }
    let report = usecase::boot_rebuild(store)?;
    for r in &report.roots {
        println!(
            "{} 目标根 #{} {}  [{}] {}{}",
            if r.ready { "✅" } else { "❌" },
            r.root_id,
            r.path,
            r.label,
            if r.ready {
                format!("等待 {}ms", r.waited_ms)
            } else {
                "未就绪".into()
            },
            r.error
                .as_ref()
                .map(|e| format!("\n      {e}"))
                .unwrap_or_default()
        );
    }
    println!();
    println!("{}", report.one_line());
    for w in &report.warnings {
        println!("  ⚠ {w}");
    }
    for (id, e) in &report.failed {
        println!("  ❌ #{id} {e}");
    }
    if report.has_problem() {
        return Err(TdError::Other("开机重建存在未就绪项".into()));
    }
    Ok(())
}

pub(crate) fn cmd_autostart(p: &Parsed) -> TdResult<()> {
    use td_core::autostart;
    match p.pos(0).unwrap_or("status") {
        "status" => {
            let st = usecase::autostart_status();
            println!(
                "{} 任务名：{}",
                if st.registered { "✅ 已注册" } else { "❌ 未注册" },
                st.task_name
            );
            println!("   {}", st.message);
            if st.registered {
                println!("   触发：用户登录时（InteractiveToken / 最低权限）");
                println!("   说明：任务里只跑 `boot-rebuild`，失败结果会落库并在界面顶部提示");
            }
            Ok(())
        }
        "enable" => {
            // `--exe` 允许指定别的执行体（默认推荐 GUI 静默二进制），
            // 所以不走 usecase::autostart_enable 的固定版本。
            let exe = match p.value("exe") {
                Some(v) => PathBuf::from(v),
                None => autostart::preferred_exe(),
            };
            let cmd = autostart::enable(&exe, &autostart::extra_args_from_env())?;
            println!("✅ 已注册开机自启");
            println!("   任务名：{}", autostart::TASK_NAME);
            println!("   命令：{cmd}");
            println!("   立即验证一次：tempdirector autostart run");
            Ok(())
        }
        "disable" => {
            println!("{}", usecase::autostart_disable()?);
            Ok(())
        }
        "run" => {
            println!("{}", usecase::autostart_run_now()?);
            println!("（任务在后台跑，几秒后可用 `tempdirector ops` 或界面「运维日志」查看结果）");
            Ok(())
        }
        other => Err(TdError::Other(format!(
            "未知的 autostart 子命令：{other}（可用：status / enable / disable / run）"
        ))),
    }
}