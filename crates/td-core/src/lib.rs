//! # TempDirector 内核
//!
//! 把上百款软件的缓存/临时目录，用 **目录 Junction**（无需管理员权限）统一重定向到
//! RAM 盘，并在每次开机后用守护逻辑把 RAM 盘上的空目录结构批量重建出来，
//! 保证那些 Junction 永远指向"存在且可写"的目录。
//!
//! ## 分层
//!
//! | 模块 | 职责 | 是否碰数据库 |
//! |---|---|---|
//! | [`util`] | 路径归一化、`\\?\` 长路径、环境变量展开、卷判定 | 否 |
//! | [`safety`] | 规则合法性黑名单（卷根/系统目录/自环） | 否 |
//! | [`link`] | 重解析点检测、Junction 建/拆 | 否 |
//! | [`fsops`] | 占用检测、跨卷安全搬运、数据核对 | 否 |
//! | [`engine`] | 编排：检测 → 搬运 → 建链 → 回滚 → 落库 | 是 |
//! | [`store`] | SQLite 规则库与操作流水 | 是 |
//! | [`boot`] | 等待卷就绪、开机批量重建、ACL 回灌 | 是 |
//! | [`autostart`] | 注册/取消"登录时自动重建"的用户级任务计划 | 否 |
//! | [`worker`] | 提权 worker 的 job 协议与执行体 | 是 |
//! | [`elevate`] | 判定提权状态、以管理员身份拉起进程 | 否 |
//!
//! ## 两条不可动摇的安全铁律
//!
//! 1. **数据核对通过之前，绝不删除源目录内容。**
//! 2. **删除链接只允许 `remove_dir`（删链接本身），任何情况下不得对重解析点递归删除。**
//!
//! 违反第 2 条会直接删掉 RAM 盘里的真实数据，因此本 crate 内**不存在**对
//! 用户路径调用 `remove_dir_all` 的代码路径。

pub mod autostart;
pub mod boot;
pub mod elevate;
pub mod engine;
pub mod error;
pub mod fsops;
pub mod link;
pub mod paths;
pub mod portable;
pub mod safety;
pub mod store;
pub mod util;
pub mod worker;

pub use error::{TdError, TdResult};
