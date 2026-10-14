//! 提权 worker：把"需要管理员权限"的动作交给一个一次性进程去做。
//!
//! ## 为什么需要它
//!
//! 目录 Junction 本身不需要特权，但**写受保护目录**（`C:\Program Files`、
//! `C:\ProgramData`、`C:\Windows` 下的缓存）需要管理员权限。
//! 让整个 GUI 常驻管理员身份是不可接受的，所以改成"按需提权"：
//! 界面上点一下 → 弹一次 UAC → 一个短命进程干完活就退出。
//!
//! ## 信任模型（这是本模块最要紧的部分）
//!
//! worker 是被提权执行的，所以必须限制它能干什么：
//!
//! 1. **job 文件里只有 rule id，没有任何路径**。
//!    即便有人伪造一份 job，也只能触发"规则库里本来就合法的动作"，
//!    不能凭空让它去删任意目录。
//! 2. **job 必须位于 `<数据目录>\jobs\` 之内**（[`validate_job_location`]）。
//!    这条把"job 从哪来"钉死在我们的数据目录上。
//! 3. worker 里**重跑全部安全校验**（走与 CLI/GUI 完全相同的
//!    `engine::apply_rule` / `engine::restore_rule` 路径，含黑名单与自环检查）。
//! 4. 结果写回 `<job>.result.json`，由调用方读取；worker 自己不做任何 UI。

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::engine::{self, ApplyOptions};
use crate::error::{TdError, TdResult};
use crate::fsops::mover::Progress;
use crate::fsops::verify::VerifyMode;
use crate::store::Store;

/// job 文件格式版本。
pub const JOB_VERSION: i32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Job {
    pub schema: i32,
    /// `apply` / `restore` / `boot_rebuild`
    pub kind: String,
    /// 只允许 rule id —— 刻意不含任何路径字段。
    pub rules: Vec<i64>,
    #[serde(default)]
    pub dry_run: bool,
    /// 目标目录非空时是否允许合并。
    #[serde(default)]
    pub allow_merge: bool,
    #[serde(default)]
    pub sha256: bool,
    /// 发起进程 pid，只用于日志。
    #[serde(default)]
    pub origin_pid: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobItem {
    pub rule_id: i64,
    pub name: String,
    pub ok: bool,
    pub action: String,
    pub message: String,
    pub elapsed_ms: u128,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobResult {
    pub ok: bool,
    pub kind: String,
    pub started_at: String,
    pub finished_at: String,
    pub items: Vec<JobItem>,
    pub error: Option<String>,
}

impl JobResult {
    pub fn one_line(&self) -> String {
        let ok = self.items.iter().filter(|i| i.ok).count();
        let failed = self.items.len() - ok;
        match &self.error {
            Some(e) => format!("worker 失败：{e}"),
            None => format!("worker 完成：成功 {ok}，失败 {failed}"),
        }
    }
}

pub fn jobs_dir() -> PathBuf {
    crate::paths::jobs_dir()
}

fn now_iso() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    // 只用于日志可读性，不参与判定；本地时间由调用方格式化
    format!("unix:{secs}")
}

/// 写一份 job 到 `<数据目录>\jobs\`，返回路径。
pub fn write_job(job: &Job) -> TdResult<PathBuf> {
    let dir = jobs_dir();
    std::fs::create_dir_all(&dir)?;
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let name = format!("{}-{}-{stamp}.job.json", job.kind, std::process::id());
    let path = dir.join(name);
    let bytes = serde_json::to_vec_pretty(job)?;
    std::fs::write(&path, bytes)?;
    Ok(path)
}

pub fn result_path_for(job_path: &Path) -> PathBuf {
    let stem = job_path
        .file_name()
        .map(|s| s.to_string_lossy().replace(".job.json", ""))
        .unwrap_or_else(|| "job".into());
    job_path.with_file_name(format!("{stem}.result.json"))
}

pub fn read_result(job_path: &Path) -> Option<JobResult> {
    let p = result_path_for(job_path);
    let text = std::fs::read_to_string(p).ok()?;
    serde_json::from_str(&text).ok()
}

/// 信任锚：job 必须在规范数据目录的 `jobs` 子目录内。
///
/// 这一条挡掉的是"把一个精心构造的 job 塞到随便某个目录、再诱导提权进程去执行它"。
pub fn validate_job_location(path: &Path) -> TdResult<()> {
    let canon = crate::util::normalize_key(&jobs_dir().to_string_lossy());
    // 先绝对化（顺带剥掉 `\\?\` 前缀）再比较 —— 否则 verbatim 路径永远匹配不上，
    // worker 会把自己写的合法 job 全部拒掉。
    let abs = crate::util::simplify(&crate::util::to_verbatim(path));
    let actual = crate::util::normalize_key(&abs.to_string_lossy());
    let prefix = format!("{canon}\\");
    if !actual.starts_with(&prefix) {
        return Err(TdError::SafetyBlocked(format!(
            "拒绝执行：job 文件必须位于 {canon}\\ 之内，实际是 {actual}。\
             （提权 worker 只接受本程序自己写出的任务文件）"
        )));
    }
    Ok(())
}

/// 执行一份 job 并把结果写回磁盘。
pub fn run_job(job_path: &Path) -> TdResult<JobResult> {
    validate_job_location(job_path)?;

    let text = std::fs::read_to_string(job_path)
        .map_err(|e| TdError::Other(format!("读取 job 失败：{e}")))?;
    let job: Job = serde_json::from_str(&text)
        .map_err(|e| TdError::Other(format!("解析 job 失败：{e}")))?;
    if job.schema > JOB_VERSION {
        return Err(TdError::Other(format!(
            "job 版本 {} 高于本程序支持的 {JOB_VERSION}",
            job.schema
        )));
    }

    // 时间戳从库里取（本地时间、可读），失败才退回 unix 秒
    let store = Store::open_default()?;
    let started_at = store.now().unwrap_or_else(|_| now_iso());
    let _ = store.log_event(
        None,
        "info",
        "worker_start",
        &format!(
            "提权 worker 开始：kind={} rules={:?} origin_pid={}",
            job.kind, job.rules, job.origin_pid
        ),
    );

    let mut items: Vec<JobItem> = Vec::new();
    let mut hard_error: Option<String> = None;

    match job.kind.as_str() {
        "apply" | "restore" => {
            let opts = ApplyOptions {
                dry_run: job.dry_run,
                verify: if job.sha256 {
                    VerifyMode::Sha256
                } else {
                    VerifyMode::CountAndSize
                },
                // 提权不做占用检测豁免：源目录正被写入时照样拒绝
                skip_lock_check: false,
                allow_merge: job.allow_merge,
            };
            for id in &job.rules {
                let rule = match store.get_rule(*id) {
                    Ok(r) => r,
                    Err(e) => {
                        items.push(JobItem {
                            rule_id: *id,
                            name: format!("#{id}"),
                            ok: false,
                            action: "not_found".into(),
                            message: e.to_string(),
                            elapsed_ms: 0,
                        });
                        continue;
                    }
                };
                let mut noop = |_: Progress| {};
                let outcome = if job.kind == "apply" {
                    engine::apply_rule(&store, &rule, &opts, &mut noop)
                } else {
                    engine::restore_rule(&store, &rule, &opts, &mut noop)
                };
                match outcome {
                    Ok(o) => items.push(JobItem {
                        rule_id: o.rule_id,
                        name: o.name,
                        ok: true,
                        action: o.action.to_string(),
                        message: o.message,
                        elapsed_ms: o.elapsed_ms,
                    }),
                    Err(e) => items.push(JobItem {
                        rule_id: rule.id,
                        name: rule.name,
                        ok: false,
                        action: "failed".into(),
                        message: e.to_string(),
                        elapsed_ms: 0,
                    }),
                }
            }
        }
        "boot_rebuild" => match crate::boot::run_boot_rebuild(&store) {
            Ok(report) => items.push(JobItem {
                rule_id: 0,
                name: "开机重建".into(),
                ok: !report.has_problem(),
                action: "boot_rebuild".into(),
                message: report.one_line(),
                elapsed_ms: report.elapsed_ms,
            }),
            Err(e) => hard_error = Some(e.to_string()),
        },
        other => {
            return Err(TdError::InvalidPath(format!(
                "未知的 job 种类：{other}（可用：apply / restore / boot_rebuild）"
            )))
        }
    }

    let result = JobResult {
        ok: hard_error.is_none() && items.iter().all(|i| i.ok),
        kind: job.kind.clone(),
        started_at,
        finished_at: store.now().unwrap_or_else(|_| now_iso()),
        items,
        error: hard_error,
    };

    let out = result_path_for(job_path);
    std::fs::write(&out, serde_json::to_vec_pretty(&result)?)?;

    let _ = store.log_event(
        None,
        if result.ok { "info" } else { "warn" },
        "worker_done",
        &result.one_line(),
    );
    Ok(result)
}

/// 运行 job 的入口（供两个二进制的 argv 分支调用）：返回进程退出码。
pub fn run_job_to_exit_code(job_path: &Path) -> i32 {
    match run_job(job_path) {
        Ok(r) => {
            if r.ok {
                0
            } else {
                2
            }
        }
        Err(e) => {
            // 失败也要把结果写出去，让调用方拿到具体原因而不是"什么都没发生"
            let result = JobResult {
                ok: false,
                kind: "unknown".into(),
                started_at: now_iso(),
                finished_at: now_iso(),
                items: Vec::new(),
                error: Some(e.to_string()),
            };
            let _ = std::fs::write(
                result_path_for(job_path),
                serde_json::to_vec_pretty(&result).unwrap_or_default(),
            );
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_job_outside_jobs_dir() {
        let outside = std::env::temp_dir().join("td_evil.job.json");
        let err = validate_job_location(&outside).unwrap_err();
        assert!(matches!(err, TdError::SafetyBlocked(_)), "{err}");
    }

    #[test]
    fn accepts_job_inside_jobs_dir() {
        let inside = jobs_dir().join("apply-1-1.job.json");
        assert!(validate_job_location(&inside).is_ok());
    }

    #[test]
    fn result_path_is_sibling() {
        let p = PathBuf::from(r"C:\data\jobs\apply-1-2.job.json");
        assert_eq!(
            result_path_for(&p),
            PathBuf::from(r"C:\data\jobs\apply-1-2.result.json")
        );
    }

    #[test]
    fn job_has_no_path_fields() {
        // 这条是防回归：job 一旦能携带路径，提权 worker 就变成任意路径操作器。
        let job = Job {
            schema: JOB_VERSION,
            kind: "apply".into(),
            rules: vec![1, 2],
            dry_run: false,
            allow_merge: false,
            sha256: false,
            origin_pid: 42,
        };
        let json = serde_json::to_string(&job).unwrap();
        for forbidden in ["path", "source", "target", "dir"] {
            assert!(
                !json.to_lowercase().contains(forbidden),
                "job 序列化结果里不应出现 {forbidden}：{json}"
            );
        }
    }
}
