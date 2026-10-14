//! 卷就绪判定。
//!
//! RAM 盘的可用时机有两种形态：
//! * **内核驱动的盘**（ImDisk / Radeon RAMDisk 的驱动模式）：boot 早期卷就在了；
//! * **桌面程序创建的盘**：要等用户登录、那个程序起来之后卷才出现。
//!
//! 所以这里不能"探测一次就放弃"，必须带超时轮询；超时后要**告警**而不是静默失败 ——
//! 否则用户第二天开机遇到的是一堆报错的软件，却不知道是守护没跑成。

use std::path::Path;
use std::time::{Duration, Instant};

use crate::error::{TdError, TdResult};
use crate::fsops::mover::free_space;

/// 卷是否就绪：目录存在 + 能查到容量 + 能真的写进去。
///
/// 只判断 `exists()` 是不够的：盘符被别的设备临时占用、卷处于只读挂载、
/// 或者 RAM 盘刚创建还没格式化，都会让"目录存在"但不写入。
pub fn is_volume_ready(path: &Path) -> bool {
    if !path.is_dir() {
        return false;
    }
    if free_space(path).is_err() {
        return false;
    }
    let probe = path.join(format!(".td_probe_{}", std::process::id()));
    match std::fs::write(&probe, b"") {
        Ok(()) => {
            let _ = std::fs::remove_file(&probe);
            true
        }
        Err(_) => false,
    }
}

/// 等待卷就绪。返回实际等待时长。
pub fn wait_for_volume(path: &Path, timeout: Duration, poll: Duration) -> TdResult<Duration> {
    let started = Instant::now();
    loop {
        if is_volume_ready(path) {
            return Ok(started.elapsed());
        }
        if started.elapsed() >= timeout {
            return Err(TdError::Other(format!(
                "等待目标卷就绪超时（已等 {} 秒）：{}。RAM 盘未创建、未格式化，或创建它的程序还没启动。",
                timeout.as_secs(),
                path.display()
            )));
        }
        std::thread::sleep(poll);
    }
}

/// 一次性的快速探测（不等待），用于 UI 上判断"RAM 盘在不在"。
pub fn probe_volume(path: &Path) -> (&'static str, String) {
    if !path.exists() {
        return ("missing", format!("目标卷不存在：{}", path.display()));
    }
    if !path.is_dir() {
        return ("not_dir", format!("目标路径不是目录：{}", path.display()));
    }
    match free_space(path) {
        Ok(free) => {
            let probe = path.join(format!(".td_probe_{}", std::process::id()));
            match std::fs::write(&probe, b"") {
                Ok(()) => {
                    let _ = std::fs::remove_file(&probe);
                    (
                        "ready",
                        format!("就绪，可用空间 {}", crate::util::human_bytes(free)),
                    )
                }
                Err(e) => ("readonly", format!("不可写入：{e}")),
            }
        }
        Err(e) => ("no_space_info", format!("无法查询容量：{e}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn temp_dir_is_ready() {
        let d = std::env::temp_dir();
        assert!(is_volume_ready(&d));
        let (state, _) = probe_volume(&d);
        assert_eq!(state, "ready");
    }

    #[test]
    fn missing_path_not_ready() {
        let d = std::env::temp_dir().join("td_volume_does_not_exist_xyz");
        assert!(!is_volume_ready(&d));
        let (state, _) = probe_volume(&d);
        assert_eq!(state, "missing");
    }

    #[test]
    fn wait_times_out_with_clear_error() {
        let d = std::env::temp_dir().join("td_volume_does_not_exist_xyz");
        let err = wait_for_volume(&d, Duration::from_millis(150), Duration::from_millis(50))
            .unwrap_err();
        assert!(err.to_string().contains("超时"), "{err}");
    }

    #[test]
    fn wait_returns_immediately_when_ready() {
        let d = std::env::temp_dir();
        let waited = wait_for_volume(&d, Duration::from_secs(5), Duration::from_millis(50)).unwrap();
        assert!(waited < Duration::from_secs(1));
    }
}
