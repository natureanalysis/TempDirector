//! 帮助文本。

pub(crate) fn print_help() {
    println!(
        r#"TempDirector —— 把软件缓存目录重定向到 RAM 盘的软链接管理器

用法：tempdirector <命令> [参数] [--json]

全局参数：
  --data-dir <路径>          覆盖数据目录（等效环境变量 TEMPDIRECTOR_DATA_DIR）

=== 概览 ===
  doctor                     环境自检：数据目录、链接能力、目标盘、水位、规则状态、待修复项
  version                    版本信息

=== 目标根（RAM 盘）===
  roots list
  roots add <名称> <路径> [--timeout 毫秒] [--hdd]
  roots rm <id>
  volume <R: 或 R:\Dir> [--timeout 毫秒]     探测 / 等待卷就绪
   water [--json]                             RAM 盘水位巡检（级别变化才落事件，
                                              阈值默认 85%/95%，settings 的
                                              water_warn_pct / water_critical_pct 可覆盖）
   water sweep <策略> [--yes]                 水位一键清理：全部规则按统一策略批量
                                              预演/执行（策略与 clean 相同：
                                              --empty / --yesterday / --older-than 1h …）

=== 规则 ===
  rules list [--enabled] [--category 分类] [--search 关键词] [--json]
  rules add <名称> <源路径模板> <目标子目录> [--root id] [--category 分类]
            [--link-type junction|symlink_dir] [--enable] [--no-boot] [--notes 备注]
            [--exclude 模式]                     清理时跳过的子目录/文件，可重复多条
                                                 如 --exclude sessions --exclude "logs\\old"
            [--auto-clean 策略]                  水位到严重线（默认95%）时的预授权清理：
                                                 empty（清空）/ older:<秒>（只清早于该秒修改的），
                                                 如 --auto-clean older:86400；缺省 = 不自动清理
  rules rm <id...>
  rules enable|disable <id...|--all>

=== 体检与操作 ===
  status [--enabled|--all] [--category 分类] [--search 关键词] [--no-persist] [--json]
  locks <规则id|目录路径> [--json]          检测占用并指出是哪个进程占着
  clean <规则id|目录路径> <策略> [--yes] [--json]
        清理文件夹（永久删除，默认只预演，加 --yes 才执行）
        策略四选一：
          --empty                        清空全部文件
          --yesterday                    昨天及更早修改的（本地零点前）
          --older-than <时长>            早于某时刻，如 1h / 30m / 7d / today / Unix秒
          --from <时> --to <时>          指定时间段 [from,to)，如 --from 7d --to 1d
  apply <id...|--all> [--dry-run] [--merge] [--sha256] [--skip-lock-check]
  restore <id...|--all> [--dry-run] [--sha256]
  relink <id>                                只重建链接（不动数据）

=== 开机重建 ===
  boot-rebuild [--json]
  autostart status                           查看"登录时自动重建"任务是否已注册
  autostart enable [--exe <路径>]            注册（用户级，不需要管理员权限）
  autostart disable
  autostart run                              立刻手动触发一次，验证配置

=== 提权 worker（一般不需要手动调）===
  elevated <job.json>                        以管理员身份执行一份 job（只认规则 id，不认路径）

=== 配置迁移 ===
  export <文件.json>
  import <文件.json> [--dry-run] [--ram-root R:]

=== 审计 ===
  events [--limit N]
  ops [--limit N]                            操作流水（含待修复项）

分类取值：browser / design / dev / system / media / office / game / other

示例：
  tempdirector roots add RAMDisk R:
  tempdirector rules add "Chrome 缓存" "%LOCALAPPDATA%\Google\Chrome\User Data\Default\Cache" browsers\chrome-cache --category browser --enable
  tempdirector status --all
  tempdirector apply --all --dry-run
  tempdirector apply 1
  tempdirector restore 1
  tempdirector clean 3 --older-than 1d          预演：规则#3 真实目录里 1 天前的文件
  tempdirector clean 3 --older-than 1d --yes    确认后执行删除
  tempdirector autostart enable
"#
    );
}