//! 统一错误类型。
//!
//! 设计原则：错误信息**本身就是给用户看的文案**（中文、带具体路径、带下一步建议）。
//! UI 与 CLI 都原样展示，不做二次包装，避免出现"操作失败（错误 0x80070005）"这种没用的提示。

use std::path::Path;

/// TempDirector 统一错误。
#[derive(Debug, thiserror::Error)]
pub enum TdError {
    #[error("IO 错误：{0}")]
    Io(#[from] std::io::Error),

    #[error("数据库错误：{0}")]
    Db(#[from] rusqlite::Error),

    #[error("JSON 处理错误：{0}")]
    Json(#[from] serde_json::Error),

    #[error("路径非法：{0}")]
    InvalidPath(String),

    #[error("安全检查未通过：{0}")]
    SafetyBlocked(String),

    #[error("被占用：{0}")]
    Busy(String),

    #[error("目标卷空间不足：需要 {need}，可用 {free}")]
    NoSpace { need: String, free: String },

    #[error("数据核对失败：{0}")]
    VerifyFailed(String),

    #[error("需要管理员权限：{0}")]
    ElevationRequired(String),

    #[error("找不到记录：{0}")]
    NotFound(String),

    #[error("{0}")]
    Other(String),
}

pub type TdResult<T> = Result<T, TdError>;

/// 把裸 IO 错误翻译成「动作 + 路径 + 建议」三段式文案。
pub fn io_ctx(e: std::io::Error, path: &Path, action: &str) -> TdError {
    let hint = match e.raw_os_error() {
        Some(32) => "（文件正被其他程序占用，请先关闭相关软件）",
        Some(5) => "（访问被拒绝，该路径需要管理员权限）",
        Some(2) | Some(3) => "（路径不存在）",
        Some(112) => "（磁盘空间不足）",
        Some(206) => "（路径过长，已启用 \\\\?\\ 前缀仍失败请缩短目录层级）",
        Some(4390) => "（不是重解析点）",
        _ => "",
    };
    TdError::Other(format!("{action}失败：{}{hint}（路径：{}）", e, path.display()))
}

/// 该 IO 错误是否属于"换个提权进程就能解决"。
pub fn needs_elevation(e: &std::io::Error) -> bool {
    // 5 = ERROR_ACCESS_DENIED，1314 = ERROR_PRIVILEGE_NOT_HELD
    matches!(e.raw_os_error(), Some(5) | Some(1314))
}

/// 该 IO 错误是否属于"被别的进程占着"。
pub fn is_busy(e: &std::io::Error) -> bool {
    e.raw_os_error() == Some(32)
}
