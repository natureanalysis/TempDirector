# TempDirector

把上百款软件的缓存 / 临时目录，用 **目录 Junction** 统一重定向到 RAM 盘，
并在每次开机后用守护逻辑把 RAM 盘上的目录结构自动重建出来 —— 从而在减少 SSD 写入的同时，
让高频读写的缓存落到内存盘上。

**核心特点：常规场景全程零 UAC 弹窗**（目录 Junction 不需要 `SeCreateSymbolicLinkPrivilege`），
只有在源路径位于 `Program Files` / `ProgramData` 等受保护位置时才按需提权。

---

## 当前状态

| 里程碑 | 内容 | 状态 |
|---|---|---|
| **M1** | 链接引擎内核 + 安全搬运 + 规则库 + 开机重建 + CLI | ✅ 已完成并通过真机端到端验证 |
| **M2** | Tauri 2 桌面界面（列表 / 搜索 / 筛选 / 多选 / 弹窗 / 详情面板 / 托盘 / 进度） | ✅ 已完成并真机启动验证 |
| M3 | 按需提权 worker + 托盘通知 | 待做 |
| M4 | Windows 服务（登录事件触发重建 + 巡检自愈）+ 命名管道 IPC | 待做 |
| M5 | 内置预设规则库（百款软件模板）+ NSIS 安装包 | 待做 |

M1 的产出是 `td-core`（内核库）与 `tempdirector`（CLI / 未来的提权 worker / 开机重建执行体）；
M2 的产出是 `td-gui`（`tempdirector-gui.exe`，界面），它**不含任何链接逻辑**，
只是一层薄壳，所有业务逻辑都从 `td-core` 来。

---

## 桌面界面

![主界面](docs/screenshots/main-window.png)

* **列表**：状态徽标（✅ 正常 / ⚠️ 断链或目标空 / ❌ 未链接 / 🚫 外来重解析点）、
  需要处理的原因直接标在名称下方、源路径与目标路径并排、期望开关与开机重建两列指示灯。
* **检索**：搜索框（名称 / 源路径 / 目标子目录）、分类筛选、只看已开启、只看需处理、全选当前结果。
* **批量**：勾选后一键「应用 / 恢复 / 开启 / 关闭 / 检测占用 / 仅重建链接 / 删除」。
* **详情面板**：选中一行展开完整信息（源模板与展开后路径、链接类型、警告、上次应用时间与规模），
  并给出该条目的单条操作按钮。
* **添加 / 编辑规则**：实时校验（路径展开结果、是否缺环境变量、是否受保护位置、目标位置是否已有内容），
  源目录可直接用系统目录选择框挑。
* **进度浮层**：批量应用时按条显示进度（来自后端 `td-progress` 事件），不会出现"点了没反应"。
* **运维日志**：事件时间线 + 操作流水两个标签页，含失败原因。
* **托盘**：显示主窗口 / 立即执行开机重建 / 退出（关闭窗口即退出，常驻形态在 M3）。

![添加规则](docs/screenshots/add-rule.png)

### 构建与运行

```powershell
scripts\cargo.cmd build -p td-gui            # 调试版（带控制台，方便看日志）
scripts\cargo.cmd build -p td-gui --release  # 发行版（无控制台窗口）
# 产物：target\release\tempdirector-gui.exe
```

界面是**静态文件**（`crates/td-gui/ui/`，编译时内嵌进 exe），**不需要 npm / 不需要打包器**。
想单独看界面（不启动 Tauri）可以直接在浏览器里打开 `crates/td-gui/ui/index.html?mock=1`，
会挂上一套内存假数据；加 `&progress=1` 看进度浮层，`&modal=1` / `&settings=1` / `&ops=1` 看各个弹窗。

---

## 已实测验证的行为

以下都在本机用真实文件系统跑过（不是单元测试的 mock）：

| 场景 | 结果 |
|---|---|
| 同卷重定向 | 走 `rename`：3 文件 / 2 MB，71ms，保留全部元数据 |
| 跨卷重定向 | 走复制 → 校验 → 删源：2 文件 / 1 MB，25ms |
| 建链结果独立核验 | `cmd dir /AL` 显示 `<JUNCTION>`；`fsutil reparsepoint query` 显示 Tag `0xa0000003`（Mount Point） |
| 通过链接读写 | 读 `a.txt` / 2 MB 二进制 / 子目录文件均正常；写入落在 RAM 盘侧 |
| **模拟 RAM 盘重启** | 链接变 `⚠️ 断裂`；`boot-rebuild` 重建目录；链接恢复为可用（目标为空，属预期）；再次写入成功 |
| 恢复（RAM 盘有内容） | 拆链接 → 内容搬回源目录 → 目标目录消失 |
| 恢复（RAM 盘已空） | 拆链接 → 重建空的原目录；`fsutil` 确认已不是重解析点 |
| 规则库导出 / 导入 | JSON 往返正确，中文规则名与路径无损 |
| 审计 | `ops` 有完整操作流水，`events` 有事件时间线 |
| **GUI 真机启动** | 窗口正常打开，内嵌前端加载、样式生效（CSP 未拦截）、IPC 通（显示真实库的 ✅/⚠️/❌ 三种状态）、托盘就位 |

---

## 快速开始

```powershell
# 1) 编译（脚本会自己补齐 MSVC + Rust 工具链环境）
scripts\cargo.cmd build --release
# 产物：target\release\tempdirector.exe

# 2) 注册 RAM 盘作为目标根
tempdirector roots add RAMDisk "R:"

# 3) 加一条规则（源路径支持 %VAR% 模板，导入库时按当前用户展开）
tempdirector rules add "Chrome 缓存" `
  "%LOCALAPPDATA%\Google\Chrome\User Data\Default\Cache" `
  "browsers\chrome-cache" --category browser --enable

# 4) 先看体检结果 + 占用情况 + 预演
tempdirector status --all
tempdirector locks "Chrome 缓存对应的规则 id"
tempdirector apply --all --dry-run

# 5) 真正执行（会先检测占用，被占用就中止且不做任何改动）
tempdirector apply --all

# 6) 开机重建（正式接法见「开机守护」）
tempdirector boot-rebuild

# 7) 想撤销就恢复
tempdirector restore --all
```

> 数据目录默认是 `%PROGRAMDATA%\TempDirector`（服务与 GUI 共用）。
> 可用环境变量 `TEMPDIRECTOR_DATA_DIR` 或参数 `--data-dir` 覆盖（便携 / 测试时有用）。

---

## 命令一览

```
概览
  doctor                     环境自检：数据目录、链接能力、目标盘、规则状态、待修复项
  version

目标根（RAM 盘）
  roots list
  roots add <名称> <路径> [--timeout 毫秒] [--hdd]
  roots rm <id>
  volume <R: 或 R:\Dir> [--timeout 毫秒]        探测 / 等待卷就绪

规则
  rules list [--enabled] [--category 分类] [--search 关键词] [--json]
  rules add <名称> <源路径模板> <目标子目录> [--root id] [--category 分类]
            [--link-type junction|symlink_dir] [--enable] [--no-boot] [--notes 备注]
  rules rm <id...>
  rules enable|disable <id...|--all>

体检与操作
  status [--enabled|--all] [--category 分类] [--search 关键词] [--no-persist] [--json]
  locks <规则id|目录路径> [--json]              检测占用并指出是哪个进程占着
  apply <id...|--all> [--dry-run] [--merge] [--sha256] [--skip-lock-check]
  restore <id...|--all> [--dry-run] [--sha256]
  relink <id>                                  只重建链接（不动数据）

开机重建
  boot-rebuild [--json]

配置迁移
  export <文件.json>
  import <文件.json> [--dry-run] [--ram-root R:]

审计
  events [--limit N]
  ops [--limit N]                              操作流水（含待修复项）
```

分类取值：`browser` / `design` / `dev` / `system` / `media` / `office` / `game` / `other`

---

## 三条不可动摇的安全铁律

1. **数据核对通过之前，绝不删除源目录内容。**
   跨卷搬运的顺序固定是：枚举 → 算空间 → 复制 → 核对（文件数 + 总字节，可选 SHA-256）→ 才删源。

2. **删除链接只允许删链接条目本身，任何情况下不得对重解析点递归删除。**
   代码里不存在对用户路径调用 `remove_dir_all` 的路径；
   `delete_tree` 会逐条判断类型，遇到重解析点只删链接。
   违反这条会直接删掉 RAM 盘里的真实数据。

3. **看不懂的东西一律不动。**
   外来重解析点（OneDrive 占位符、`AppExecLink`、卷挂载点）只报告、不处理；
   软件更新把链接覆盖成真实目录后，开机守护**不会**自作主张去搬运，只报警告交给人处理。

细节（含 20+ 条边界条件）见 [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md)。

---

## 开机守护怎么接

M1 提供执行体 `tempdirector boot-rebuild`，触发方式推荐：

* **推荐**：后台服务（M4）监听 `WTS_SESSION_LOGON`，用该用户的令牌执行 ——
  这样 `%APPDATA%` 展开正确、目录继承的 ACL 也对，而且**早于 `HKCU\Run` 里的软件启动**。
* **过渡**：任务计划程序，触发器「登录时」，操作 `tempdirector.exe boot-rebuild`。
* 等不到 RAM 盘就绪时，命令会按配置的超时（默认 90s）轮询并最终**报错退出**，
  而不是静默失败 —— 让用户能立刻发现问题，而不是第二天面对一堆报错的软件。

---

## 目录结构

```
crates/
  td-core/                 内核库（无 GUI 依赖）
    src/util.rs            路径归一化、\\?\ 长路径、环境变量展开、卷判定
    src/safety.rs          规则黑名单（卷根 / 系统目录 / 自环）
    src/link/              重解析点识别、Junction 建/拆
    src/fsops/             占用检测(+Restart Manager)、安全搬运、合并、核对
    src/engine.rs          编排：决策树 → 搬运 → 建链 → 回滚 → 落库
    src/store/             SQLite schema / 迁移 / DAO / 操作流水
    src/boot/              等待卷就绪、开机批量重建、DACL 回灌
    src/portable.rs        配置导入导出
  td-cli/                  tempdirector.exe（管理 / 提权 worker / 开机重建）
  td-gui/                  tempdirector-gui.exe（Tauri 2 界面）
    src/api.rs             业务层：只吃 &Store、吐 serde 结构体（可脱离窗口单测）
    src/commands.rs        #[tauri::command] 薄转发 + 进度事件 + 目录选择框
    src/lib.rs             入口：降级到内存库 + mark_stale_ops + 注册命令
    src/tray.rs            托盘菜单（显示窗口 / 立即重建 / 退出）
    ui/                    index.html / style.css / app.js（静态文件，编译时内嵌）
docs/ARCHITECTURE.md       架构与边界条件
docs/screenshots/          界面截图
scripts/cargo.cmd          开发构建包装（自动补 MSVC + Rust 环境）
scripts/make_icon.py       应用图标生成（纯 PIL 绘制，可重跑）
```

---

## 开发

```powershell
scripts\cargo.cmd check --workspace --all-targets
scripts\cargo.cmd test  --workspace          # 87 个单元测试（td-core 75 + td-gui 12）
scripts\cargo.cmd build --release            # 两个 exe 一起出
python scripts\make_icon.py                  # 需要改图标时重跑
```

`scripts/cargo.cmd` 存在的理由：MSVC 工具链需要先跑 `vcvars64.bat`，
而受限 shell 里 `PATH` 可能是空的、`RUSTUP_HOME` / `CARGO_HOME` 也可能没指对。
脚本会把环境补齐后再转发给 cargo。

（该文件必须保持纯 ASCII —— `cmd.exe` 用 OEM 码页读批处理，中文注释会让它解析崩溃。）

### 关于 tauri 版本：整个家族必须锁在同一代

只写 `tauri = "2.11.5"` 是不够的。`tauri-codegen` / `tauri-macros` / `tauri-utils` /
`tauri-runtime` 都是**范围依赖**，会各自解析到更新的版本，结果是
`tauri-macros` 生成的重试码引用了 `tauri` 本体里不存在的错误变体，
编译直接爆出 27 个 `no variant named UnexpectedMenuKind` 之类的错。
解法是用 `cargo update -p <crate> --precise <ver>` 把锁文件对齐到一套已验证组合
（`tauri 2.11.5 / tauri-build 2.6.3 / tauri-codegen 2.6.3 / tauri-macros 2.6.3 /
tauri-plugin 2.6.3 / tauri-utils 2.9.3 / tauri-runtime 2.11.3 / tauri-runtime-wry 2.11.4 / wry 0.55.1`）。
**升级 tauri 时要整套一起动，不要只改本体那一个版本号。**
