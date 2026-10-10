# AICRM Windows API

> **2026-10-10 变更（重要）**：**设备绑定 / 配对已彻底删除**。没有 `device.id` /
> `device.token` / 预置码，也没有设备表与会话表；云端只认「客户端有没有登录与云端
> 相同的账号」。登录同一账号即可直接控制客户端；一个账号只能登录一个客户端。
> 本文档下方仍可见「设备」「配对」字样的段落属**历史记录**（更新记录一节保留原样），
> 当前行为以《`src/support/项目架构.md`》与本节为准。
> 另外：安装器现在会先做**环境准备**（系统版本 / 磁盘 / 权限 / 运行库 / 云端连通性），
> 全部必须项通过才允许安装；我们自己的程序已改为**静态链接 CRT**，客户机不需要预装 VC++。

作者：Tang。当前 API 只依赖两个辅助程序：Windows 0.4.7 的 `AICRM-Key.exe`（取密钥、列本地账号）与 `aicrm-export-new.exe`（导出 `messages.md`，人工查阅与云端 AI 分析共用同一份 Markdown）。导出程序文件来自用户提供的 `4.1.13.65 -4.1.15.13 聊天记录导出程序`，不修改原辅助程序。原先产 `messages.json` 的 `AICRM-Export.exe` 已不再需要：2026-10 起云端归档与 AI 分析输入都直接用 Markdown，JSON 中转环节整体删除。Rust 服务在**当前用户登录后**由 Windows `HKCU\...\Run` 启动；微信数据和密钥属于当前用户，因此无需以 SYSTEM 身份运行。

## 程序清单

`Cargo.toml` 声明五个二进制（其中 `aicrm-pack` 是打包工具，不随客户端分发）：

| 二进制 | 入口 | 作用 | 用法 |
| --- | --- | --- | --- |
| `aicrm_windows_api` | `src/main.rs` | 后台服务：HTTP 接口 + 云端命令通道。参数全部可选，默认从 exe 同目录读配置 | 无界面，由 `HKCU\...\Run` 隐藏启动 |
| `aicrm-console` | `src/forckend/console.rs` | 主窗口：侧边栏 + 状态总览（云端 / 密钥 / 服务）+ 配置页 + 账号菜单；**登录 / 重新登录 / 首次「连接云端」配对都在这一窗口内完成**；**托管本地服务**；托盘形态由 `--hidden` 提供（原独立托盘程序已并入） | 无参数；`--hidden` 只进托盘不弹窗；`--start-service` 只起服务不显示窗口 |
| `aicrm-update` | `src/update/main.rs` | **原生更新器**（取代 PowerShell 脚本）：校验摘要与目标名白名单 → 先停客户端进程 → 备份 → 覆盖 → 按 `startup.json` 重启 → 探活失败回滚；也承担快捷方式创建（`--ensure-shortcut`）与旧程序清理 | 由服务在应用更新时拉起；`--ensure-shortcut` 供安装脚本调用 |
| `aicrm-setup` | `src/forckend/setup.rs` | **静默安装**：不问任何东西，打开即装盘 → 装完自动拉起客户端 → 自己退出 | `--endpoint`；`--extract-only` 自检内嵌载荷。单文件形态由 `aicrm-pack` 拼出 |
| `aicrm-pack` | `src/pack/main.rs` | 打包工具：安装器 + 全部载荷 → 单文件安装程序 | `cargo run --release --bin aicrm-pack [输出路径]` |

（`aicrm-login.exe` 登录窗口已于 2026-10-10 并入主控台，不再分发。）

四件事值得记一笔：

- **外观统一**：`src/forckend/theme.rs` 是两个窗口（主控台、安装器）共用的外观（浅色底、白卡片、描边、留白），各自 `mod theme;` 指向同一份文件，不引入共享库。跨二进制共享的无界面工具统一在 `src/shared/`（路径约定 / 设备凭据 / 进程管理 / 字体 / 缓存探测 / 机主昵称），各二进制按需引入。
- **主窗口不依赖服务**：它直接读安装目录里的文件（令牌、密钥、`cloud.url`、`data/jobs`），因为它的职责之一就是"服务没跑时把服务拉起来"。宽高固定、长文本截断，避免窄窗口把中文挤成竖排。
- **单文件安装程序**：`aicrm-pack` 把 `aicrm-setup.exe` 与载荷 zip 拼在一起，页脚 `[offset u64][len u64]["AICRMSFX"]`；安装器启动时读自身末尾定位并解压载荷，没有页脚就退回"同目录"布局。载荷清单在 `src/pack/main.rs` 顶部。输出名是 `AICRM-<版本号>.exe`，版本号取自 `Cargo.toml`，不用手工同步。
- **一个时间只有一个界面**：`src/forckend/single.rs` 用命名互斥体 `Local\AICRM.WinClient` 做单实例，主界面与安装器**共用同一把锁**，所以两者不会并排出现。重复启动时把已有窗口 `SW_RESTORE` + 置前，而不是再开一个。开窗前先 `acquire()`；安装器装完要拉起主界面前必须 `release()`，否则主界面会被自己那把锁挡掉（表现为"装完了但客户端弹不出来"）。
- **打开客户端 = 服务就该在跑**：主窗口启动时托管 `aicrm_windows_api.exe`，每 5 秒的刷新里检查一次，没起就拉起、掉了就重启。用户手点「停止服务」时置 `auto_service=false` 不再自动拉起，点「启动服务」恢复。服务是采集和心跳的载体，它一掉云端立刻把设备标离线，网页端也就点不动「绑定微信」。

托盘（`aicrm-console --hidden` 的形态）图标颜色就是服务状态（绿=运行中、灰=已停止），菜单里「令牌」一项看 `api.token` 是否非空；「服务在不在」是直接连 `--bind` 端口判定的，比查进程名更能说明"到底能不能用"。

## 安装与运行

在本项目目录运行 `./install.ps1`。脚本编译 Rust 程序，把客户端三件套（`aicrm_windows_api.exe` / `aicrm-console.exe` / `aicrm-update.exe`）、`AICRM-Key.exe` 与 Markdown 导出程序放到 `%LOCALAPPDATA%\AICRM-Windows-API\`，生成权限限定为当前用户、SYSTEM 和管理员的 `api.token`，注册登录自启并立即启动。现有密钥仍读取本项目 `src\tools\accounts\`，不会复制或删除。服务默认监听 `127.0.0.1:8765`。需要更换密钥目录或监听地址时使用 `./install.ps1 -KeyDir 'D:\密钥目录' -Bind '127.0.0.1:8765'`（这两个非常规值会自动写进自启项参数；常规部署自启项只有一行 exe 路径）。重新运行安装脚本会更新程序并重启服务，不会更换已有 token。

### 账号即客户端（2026-10-10 起，设备层已删除）

**没有「设备」这个东西。** 云端只认一件事：**客户端有没有用某个账号登录着**。
配对、设备令牌、预置码、设备归属全部删除（云端迁移 `004_account_client.sql` 已删除
`aicrm_agent_devices` / `aicrm_agent_device_sessions` 与 `wechat_accounts` 的 `agent_*` 列）。

```text
客户端登录账号 A → api.token 落盘（未过期的云端会话 JWT）
        ↓ 下一个心跳（≤20 秒）
PUT /api/agent/v1/status
    Authorization: Bearer <A 的登录令牌>
    X-AICRM-Client-ID: <本机生成的客户端实例标识 client.id>
        ↓
云端登记 / 刷新「账号 A 的客户端」在线状态（aicrm_agent_clients，一个账号一行）
        ↓
网页端用 A 登录就能看到这台客户端，可以绑定、可以采集

**一个账号只能登录一个客户端。** 第二台电脑用同一账号登录时，第一台仍在线则心跳被拒
（HTTP 409「该账号已在另一台电脑登录」）；第一台离线超过 2 分钟后，第二台自动接管 ——
换机、重装都不需要人工介入。`client.id` 只是本机生成的实例标识，不是凭据。
```

由此得到的行为，**完全不需要实施人员介入**：

| 场景 | 结果 |
|---|---|
| 客户端登录 A，网页也登录 A | 网页看得到「A 的客户端」，可绑定、可采集 |
| 客户端换成 B | 下一个心跳（≤20 秒）即归到 B；网页用 B 登录就能看到并操作，**A 侧随即看不到这台客户端** |
| 网页登录从没在这台机器上登录过的 C | 看不到任何客户端，采集按钮置灰 |

设计要点：

- **身份就是登录令牌。** 不再有设备凭据：命令通道与网页端用同一套 JWT，云端只按
  `user_id` 组织在线状态与命令队列（`aicrm_agent_commands.user_id`）。
- **换账号不需要任何额外动作。** 重登一次、服务重启读新令牌即可，没有配对、没有归属字段要改。
- **令牌过期 = 未登录。** 本地按 `exp` 判断（`src/shared/session.rs`），界面会引导重新登录，
  而不是显示"已登录"却什么都点不动。
- **文件是唯一事实来源**：`cloud.url`（云端地址）、`api.token`/`api.refresh`（登录凭据）、
  `client.id`（客户端实例标识，非凭据）。

### 安装：只放文件，不要账号

**安装**只负责放文件、注册开机自启，不需要任何账号：

```powershell
./install.ps1 -CloudUrl <云端基地址> [-AllowOrigin <网页域名>]
```

登录由客户端首次打开时的「连接云端」引导页完成（输入云端账号密码即可，**没有配对这一步**）。

安装前会先做**环境准备**：`install.ps1 -CheckOnly` 输出一行一条
`CHECK|级别|标题|pass/fail|说明`；安装器把它画成清单，必须项没过就不放行。
无人值守时也可以用 `AICRM-安装程序.exe --check`（写日志 + 原生提示框）。

`-CloudUrl` 会写进 `%LOCALAPPDATA%\AICRM-Windows-API\cloud.url`，隧道地址变了只改这个文件再重启服务，不用重装。

两个坑：

- 覆盖文件前脚本会停掉 `aicrm_windows_api` / `aicrm-console` / `aicrm-login`（旧版残留，顺带清理），否则 `Copy-Item` 报「文件正由另一进程使用」。
- **如果客户端正在运行，安装程序不会自己冒出来**：它会把已有窗口叫到前台并弹一句提示
  （"请先从托盘菜单退出 AICRM 再装"）。这条以前是静默退出的，看起来就是"双击安装程序没反应"。

采集闭环只依赖 `aicrm-export-new.exe`，它由 `install.ps1 -ExportProgram` 复制进 `tools\`，也是 `Service::new` 的启动校验项，缺它服务起不来。不再依赖 `AICRM-Export.exe`。

**客户机不需要预装 VC++ 运行库**：我们自己的程序已静态链接 CRT（`.cargo/config.toml`）；
改不了链接方式的第三方程序（`AICRM-Key.exe` / `aicrm-export-new.exe`）靠随包附带
`vcruntime140.dll` 到 `tools\` 与安装目录解决（应用本地装配，免管理员、免联网、免重启）。

### 换云端 / 重装前必读：一切以文件为准

**云端地址与登录凭据只有一个事实来源 —— 安装目录里的文件**（`cloud.url`、`api.token`、`api.refresh`）。服务默认从自己所在目录读它们，且**文件存在时优先于任何命令行参数**；自启项只写一行 exe 路径、不带参数。所以「改了文件不重装没用」这类坑从结构上不存在了（2026-10-10 重构起）。

- **换隧道地址**：改 `cloud.url` 再重启服务（或等服务下一轮心跳，每轮都重读），不用重装。
- **彻底换云端**：改 `cloud.url` 指向新地址，在客户端「连接云端」页用新账号登录一次。
- **重装不需要重新配对**（已经没有配对这东西），但 **`install.ps1` 仍要带 `-CloudUrl`**，否则 `cloud.url` 不会被写。
- 老版本装出来的机器：自启项里可能还带着旧参数——服务按"文件优先"处理，它们不再能造成不一致；下次重装 / 修复安装时自启项会被重写为裸路径。

验通标准：云端设备列表里这台从 `never_seen` 变成 `offline` / `online`。仍是 `never_seen` 就看 `client.err.log` 有没有 `401 Unauthorized`。

`./uninstall.ps1` 只删除登录自启项并停止服务，保留程序、密钥、token 和导出文件。手动启动命令可查看 `install.ps1` 写入的 `HKCU:\Software\Microsoft\Windows\CurrentVersion\Run\AICRM-Windows-API` 值。

## 登录（主控台内）

登录**不再是独立程序**（`aicrm-login.exe` 已于 2026-10-10 并入主控台）：主控台的「连接云端」页 —— 首次打开未配对时自动出现，也可从右上角账号菜单「重新登录」进入 —— 完成全部登录动作：输入账号密码、向云端换取**会话令牌**、写进本地令牌文件供后台服务鉴权。**密码完全由云端校验**，本地不保存、不比对 —— 本地校验等于在客户端藏一份密码哈希，可被替换或绕过。

- 云端地址取自 `cloud.url`（首次安装由 `install.ps1 -CloudUrl` 写入；本机联调可手动改）。
- 登录成功后一次写好三个文件：`account.name`（界面显示"谁登录了"）、`api.token`（会话令牌，尽力用 `icacls` 收紧权限到当前用户 / SYSTEM / Administrators）、`api.refresh`（续期用）。
- **已配对的机器**：登录到此为止，服务会在几秒内自动重启换用新令牌（服务启动时读一次令牌）。
- 登录成功后服务会在几秒内重启换用新令牌；换账号就是重新登录一次，没有其它步骤。
- 退出登录：账号菜单「退出登录」换一把新的本地令牌、清掉账号名，并**删掉 `api.refresh`** —— 不删的话续期任务会在 30 分钟内用刷新令牌把用户悄悄"登回来"（2026-10-10 修的 bug）。

### 云端登录接口契约

客户端发起的这个请求**不带 `Authorization` 头** —— 它就是"用密码换令牌"的入口，不可能要求令牌。代价是它天然是公开端点，防滥用只能靠**强制 HTTPS + 云端失败次数限制**，客户端无能为力。

```http
POST <endpoint>
Content-Type: application/json

{"username":"zhangsan","password":"…","client_id":"3f2a…","client_version":"0.1.0"}
```

成功 `200 OK`（`{code, message, data}` 信封，令牌在 `data` 里）：

```json
{"code":0,"message":"success","data":{
  "accessToken":"eyJ…","refreshToken":"eyJ…",
  "user":{"user":{"name":"张三","username":"zhangsan"}}}}
```

失败（`message` 会原样显示给用户）：

```json
{"code":401,"message":"账号或密码错误","data":null}
```

客户端要求 `code == 0` **且**响应里带 `data.accessToken`；缺 `data` 或信封形状不对会报「云端返回的不是 {code,message,data} 信封 / 云端返回成功但缺少 data」，那属于前后端版本不匹配，不是凭据问题。`refreshToken` 会落盘到 `api.refresh`，由后台服务每 30 分钟自动续期（见「云端命令通道 · 令牌自动续期」）。客户端把 `4xx` 当作"凭据错误、重试无用"，把 `5xx` 与网络故障当作"可重试"。

`client_id` 是**本机生成的客户端实例标识**（`client.id`，非凭据）：云端拿它落实「一个账号
只能登录一个客户端」。它不参与鉴权 —— 鉴权只认 `api.token` 里的登录令牌。

### 本地联调

云端还没就绪时，可用仓库根目录的两个假云端脚本跑通两条链路。

`mock-cloud.ps1` 答登录接口（账号 `demo` / 密码 `123456`，其它组合返回 401）：

```powershell
Start-Process pwsh -ArgumentList '-NoProfile','-File','.\mock-cloud.ps1','-Port','9413','-LogFile',"$env:TEMP\req.txt" -WindowStyle Hidden
# 让主控台连到这个假云端，然后打开主控台的「连接云端」页登录：
Set-Content "$env:LOCALAPPDATA\AICRM-Windows-API\cloud.url" 'http://127.0.0.1:9413' -NoNewline
start .\target\debug\aicrm-console.exe
```

`req.txt` 会记下客户端实际发出的原始请求，便于核对契约。

`mock-agent-cloud.ps1` 答命令通道，用来在没有真云端时验证「云端下发命令 → 客户端执行 → 回传结果」：

```powershell
Start-Process pwsh -ArgumentList '-NoProfile','-File','.\mock-agent-cloud.ps1','-Port','9414' -WindowStyle Hidden
./target/release/aicrm_windows_api.exe --cloud-url http://127.0.0.1:9414 --device-token dev-token --device-id dev-1 --bind 127.0.0.1:8765
```

它只下发一次 `wechat.export`（`-MaxCommands` 可调），收到结果后退出。它不实现心跳端点 `/api/agent/v1/status`，也不支持 `wechat.bind` / `wechat.collect`，因此服务日志里会出现心跳 404 —— 只影响日志，不影响那条导出命令的往返。

## 接口

除下面单列的握手端点外，所有接口都要求 `Authorization: Bearer <token>`，token 在 `%LOCALAPPDATA%\AICRM-Windows-API\api.token`。接口层只解析请求、调用 `backend/service/` 的函数。该目录按辅助程序拆分：`service/mod.rs` 是共享底座（服务状态、进程调用、并发闸门、`authorize` 鉴权中间件），`service/key_service.rs` 承载 `AICRM-Key.exe` 业务（密钥准备、账号查询），`service/export_service.rs` 承载 `aicrm-export-new.exe` 业务（Markdown 导出执行），`service/archive_service.rs` 承载 Markdown 归档打包与上传（数据采集闭环上半段），`service/agent_service.rs` 是云端命令通道（长轮询 + 心跳）。跨二进制共享的无界面工具在 `src/shared/`（路径约定 / 设备凭据 / 进程管理 / 缓存探测 / 机主昵称）。

鉴权不在各模块内单独实现，而是在 `backend/mod.rs` 的 `app()` 里通过唯一的 `route_layer` 统一施加到全部已注册路由上。新增模块只需写一个 `pub fn router() -> Router<Arc<Service>>` 并在 `app()` 中 `merge` 一行，其所有接口自动受保护，无需写任何鉴权代码；所有 `merge` 都必须写在 `route_layer` 之前。用 `route_layer` 而非 `layer`，是为了让未匹配的路径保持 404 而不是变成 401。`backend/mod.rs` 的测试会遍历表里全部 8 个接口，确认无 token、错 token、其它认证方案均返回 401。

| 方法 | 路径 | 功能 |
| --- | --- | --- |
| GET | `/api/aicrm/key/version` | Key 版本 |
| GET | `/api/aicrm/export/version` | Export 版本 |
| GET | `/api/aicrm/key/help` | Key 命令行帮助 |
| GET | `/api/aicrm/export/help` | Export 命令行帮助 |
| GET | `/api/aicrm/accounts` | 列出本地已保存密钥的 wxid，不返回密钥 |
| POST | `/api/aicrm/prepare` | 验证或捕获密钥；缺少有效密钥时辅助程序可能重启微信 |
| POST | `/api/aicrm/export` | 运行导出，文件保留在本地 `data\jobs\`，返回结果与路径 |
| POST | `/api/aicrm/collect` | 数据采集：后台导出、合并归档包并上传云端；**立即返回 202，不等导出跑完**（当前由云端下发的 `wechat.collect` 触发；本机接口保留兼容） |

导出请求示例：`{"wxid":"wxid_xxx","day":14}`。`wxid` 可省略，由辅助程序识别当前账号；`person` 可限定联系人或群名；`day` 为含今天的最近 N 天；`start` 与 `end` 必须同时提供，且不能和 `day` 同用。`task_id`、`wechat_dir` 可选；`export_dir` 是云端指定的落盘目录，给了就直接导到那里（不清理、不删），不给才用 `data\jobs\<id>` 临时目录。请求体带 `deny_unknown_fields`，字段名写错会立刻 422 而不是被静默忽略。未指定联系人和日期即导出全部历史。每个请求使用独立导出目录，所以多个导出可并发；密钥准备操作与导出互斥（同一个 `RwLock` 闸门）。

`/api/aicrm/collect` 的请求体是另一套字段，全部必填：`task_id`、`execution_token`、`user_token`、`upload_url`（形如 `https://云端/api/tasks/<taskId>/upload`），加上与导出同名的可选筛选 `wxid` / `person` / `day` / `start` / `end`。它**不能在请求里等到底**：导出要跑几分钟，挂在请求里的话调用方一断开连接、handler 就被丢弃，采集被一起取消（归档目录写到一半不动、云端任务永远停在 `created`，实测踩过）。所以它只把活儿丢进后台就回 202，进度由客户端上报，网页轮询 `GET /api/tasks/{id}` 查看。上传用 multipart，字段名必须是 `file`，并同时带 `X-Execution-Token` 与 `Authorization: Bearer <user_token>` —— 只带执行令牌会被云端 401。

### 握手端点与 CORS

| 方法 | 路径 | 鉴权 | 功能 |
| --- | --- | --- | --- |
| GET | `/api/aicrm/session` | 无（改为校验 `Origin`） | 把本机令牌交给浏览器里的 AICRM 前端 |

浏览器里的前端读不到 `api.token` 文件（网页没有文件系统权限），所以需要一个握手端点把令牌交给它。它刻意挂在鉴权层之外，但不等于敞开：**只有 `Origin` 在白名单里才返回令牌**，否则 403 `FORBIDDEN_ORIGIN`。浏览器一定会带 `Origin` 且页面脚本改不了它，所以恶意网页拿不到；本机上的其它进程本来就能直接读令牌文件，威胁面没有变大。响应只含 `token` 与 `base_url`，不含任何业务数据。

来源白名单默认是四个本机回环地址（`http://localhost:5000`、`http://127.0.0.1:5000`、`http://localhost:3000`、`http://127.0.0.1:3000`）。**部署到客户机器时必须加**：客户的网页是云端的 HTTPS 域名。用 `--allow-origin <域名>`（可重复）追加，或设环境变量 `AICRM_ALLOWED_ORIGINS`（逗号分隔）；命令行值在环境变量值之后追加去重。`install.ps1` 的 `-AllowOrigin` 就是往自启项里写这个参数。

CORS 中间件包在最外层（比鉴权还外）：预检 `OPTIONS` 不带 `Authorization`，交给鉴权层只会得到 401，所以由中间件直接回 204。响应只在来源被放行时回 `Access-Control-Allow-Origin`（刻意不用 `*`，接口需要凭据），并回 `Access-Control-Allow-Headers: Authorization, Content-Type`、`Access-Control-Max-Age: 600` 与 `access-control-allow-private-network: true` —— 最后这个头是给「公网 HTTPS 页面访问本机 127.0.0.1」用的，Chrome 会先发预检并要求它，不回的话请求在浏览器里就被拦掉，服务端连日志都没有。

### 命令行参数（后台服务）

**全部可选**：缺省时从 exe 所在目录读 `tools\` / `accounts\` / `data\` / `api.token` / `cloud.url` / `device.token` / `device.id`。云端三项凭据（地址 / 设备令牌 / 设备 ID）**文件存在时优先于任何命令行参数** —— 文件是唯一事实来源，老自启项里钉着的参数不会再和文件打架。

| 参数 | 默认值 | 说明 |
| --- | --- | --- |
| `--tools-dir` / `--key-dir` / `--data-dir` / `--token-file` | 可执行文件同级的 `tools\` / `accounts\` / `data\` / `api.token` | 目录与令牌位置 |
| `--bind` | `127.0.0.1:8765` | 监听地址 |
| `--allow-origin` | 见上 | 追加网页来源白名单，可重复 |
| `--cloud-url` / `--cloud-url-file` | exe 旁 `cloud.url`（存在时自动读） | 云端基地址。**隧道地址会变时改文件即可**：每轮轮询都重读，不用重装 |
| `--device-token` / `--device-token-file` / `--device-id` | — | **已废弃**：设备绑定已取消，只为兼容老版本写下的自启项而解析，值被忽略 |

### 云端命令通道

客户端装在客户的 Windows 电脑上，位于 NAT 之后、没有公网 IP，云端无法主动连进来，所以方向反过来：客户端定时向云端取命令，执行完回传。**云端地址齐全 + 已登录**（`cloud.url` 有内容，且 `api.token` 是未过期的云端会话）才启用轮询；否则只跑本地 API。

- 心跳：每 20 秒 `PUT <cloud>/api/agent/v1/status`。云端靠它判断设备在线，没有心跳网页端就不允许对这台设备下发采集任务。`wechat.online` 要求微信进程在跑、wxid 已绑定、本地库在、密钥就绪，任一不成立就如实报 `false`。
  **鉴权就是登录令牌本身**（`Authorization: Bearer`，每次现读 `api.token`）—— 这决定了网页端登录哪个账号才看得见这台客户端，详见上方「账号即客户端」。
- **令牌自动续期**（2026-10-10 起）：登录时 refresh token 落盘 `api.refresh`；服务每 30 分钟换新 access token 并写回。此前只存 access token（2 小时过期），过期后心跳里的会话登记与绑定拉取全部失效（表现为网页端设备"登录账号"停更、控制台误报未绑定）。
- 取命令：`POST <cloud>/api/agent/v1/commands/poll` 长轮询，挂起 `waitSeconds=25` 秒。
- 回传：`PUT <cloud>/api/agent/v1/commands/<id>/result`。
- 命令类型四种：`wechat.bind`（取密钥，可能重启微信，云端只应在确认客户在场时下发）、`wechat.export`（导出，结果留在本机）、`wechat.collect`（导出 + 归档上传，结果交回云端）、`agent.update`（自更新，见下方章节）。**采集当前由云端下发**：网页点「数据采集」→ 云端向绑定设备入队 `wechat.collect`，不再由浏览器直连本机；本机 `/api/aicrm/collect` 接口保留兼容。不支持的类型回 `UNSUPPORTED_COMMAND`，不会中断轮询。
- 这三个请求都用 `Authorization: Device <token>` + `X-AICRM-Device-ID` 两个头，与本地 API 的 `Bearer` 不是同一套。

## 本机调用

```powershell
$base = 'http://127.0.0.1:8765'
$token = [IO.File]::ReadAllText((Join-Path $env:LOCALAPPDATA 'AICRM-Windows-API\api.token')).Trim()
$headers = @{ Authorization = "Bearer $token" }
Invoke-RestMethod "$base/api/aicrm/accounts" -Headers $headers

$body = @{ day = 14 } | ConvertTo-Json -Compress
Invoke-RestMethod "$base/api/aicrm/export" -Method Post -Headers $headers `
  -ContentType 'application/json; charset=utf-8' `
  -Body ([Text.Encoding]::UTF8.GetBytes($body))
```

2026-09-29 本机真实接口测试：两个导出请求同时执行成功；`chat.txt` 有 2,527 条消息行、74,413 字符，无 `<voicemsg>` XML；语音文件引用均可在 ZIP 中找到。密钥接口用 `no_restart=true` 验证成功，返回值不含密钥正文；`/export` 的本地文件模式返回 185 条消息、5 个 JSON，结果保留在其 `job_dir`。所有帮助接口均返回 200。

（该次测试涉及的 `/export/stream`、`/export/bundle`、`/export/ai-bundle` 三个"下载原始数据"接口，经 2026-10-10 审计确认前端与云命令通道都不再调用，已删除 —— 需要时从 git 历史取回；`/export`（本地文件）与 `/collect`（采集上传）保留。）

## 更新记录

### 2026-10-10（重构：去重复 · 合并登录窗 · 零参数化 · 删除死接口）

一次刻意"不改变行为"的结构重构，按步提交、可逐步回退：

- **共享层收敛**：新增 `src/shared/`（路径约定 / 设备凭据 / 进程管理 / 字体 / 缓存探测 / 机主昵称），替换原先散在 4-5 处的副本。杀进程名单从 5 处收敛为 1 处；"服务与界面各写一份、改的时候两边都要改"这类注释全部消失。
- **登录窗合并进主控台**：删除 `aicrm-login.exe` 独立二进制（约 530 行）与"连点几次堆出多个登录窗"的 `FindWindowW` 查重逻辑；登录 / 重新登录 / 首次配对统一走「连接云端」页。**顺手修掉一个 bug：退出登录只换 `api.token` 不删 `api.refresh`，服务的 30 分钟续期会把用户悄悄"登回来"。**
- **服务零参数化**：服务默认从 exe 同目录读一切，且**文件优先于命令行**；自启项只写一行 exe 路径。删除主控台里 70 余行参数拼装与改注册表代码。「设备身份两处必须一致」的坑从结构上消失。
- **删除三个已无调用方的接口**：`/export/stream`、`/export/bundle`、`/export/ai-bundle`（审计依据见对应提交，约 190 行）；`async-stream` 依赖一并移除。
- **死代码清扫**：未使用的常量与函数（`CLIENT_PROGRAMS`、`pair_device`、`default_refresh_file`、`open_main_window`、永为 `None` 的安装器参数等）删除；`aicrm-tray.exe` 残留引用清零；编译警告归零。
- **迁移兼容**：旧自启项里的参数仍被接受（只是不再生效于文件已有值时）；升级到新版时更新器会自动清理已下线的旧程序；`aicrm-login.exe` 保留在过渡期进程名单里，覆盖"更新瞬间旧登录窗还开着"的机器。

### 2026-10-10（自更新全自动 · 整包 · 令牌续期 · 采集可靠性）

- **自更新不再等人工**：下载校验就绪后，服务等「重活计数（采集 / 导出 / 绑定）归零 + 15 秒回执余量」自动拉起更新器重启应用；「下次启动」仅作兜底。0.1.33 起生效，更早版本仍需人工重启一次。
- **整包更新**：一个版本发布 服务 / 界面 / 登录 三件套，逐件替换（白名单兜底）；旧客户端仍只收单件。界面版本号改读服务写入的 `data/state/version.json` —— 只有「真实在跑的版本」一个来源。
- **`pending.json` 只升不降**：拒绝更低版本写入，防止旧命令覆盖新标记。
- **下载超时 45s→600s**：整包里的界面程序近 10 MB，原超时必误杀。
- **登录令牌自动续期**：登录落 `api.refresh`，服务每 30 分钟续期写回；修复「2 小时过期后设备归属停更 / 控制台误报未绑定」。
- **采集可靠性**：前置检查失败（微信未运行 / 密钥不齐）也回传云端，不再留僵尸任务堵住后续采集；导出 / 打包失败同样回报。
- **密钥链路修正**：解密缓存改认 `accounts/cache/*.db`（此前查错路径导致设备永远判未就绪、采集下发不了）；换号后取「最近抓到」的账号目录；`prepare` 复用收紧（仅核对模式或定向匹配才复用，其余真抓）。

### 2026-10-09（本轮）

**去掉独立安装界面。** 以前客户装个客户端要先填云端地址、账号、密码 —— 账号密码在这里只是用来换配对码，根本不是"安装"该问的东西。现在拆开：

| | 谁做 | 用户看到什么 |
| --- | --- | --- |
| 安装 | `install.ps1`（静默） | 双击 → 自动装 → 客户端弹出 → 安装器自己消失 |
| 配对 | 客户端「连接云端」引导页 | 第一次打开时填一次账号，之后不再出现 |

- `setup.rs` 删掉表单与「开始安装」按钮，`logic()` 里 `Idle` 即自动开始；只有失败时才出现「重试 / 关闭」。
- `console.rs` 新增 `Page::Connect`：`paired()` 判 `device.token` 是否存在，未配对时首屏就是引导页（此时不画侧边栏 —— 那会儿导航没有意义）。
- `cloud.rs` 新增 `Provision` / `create_provision` / `pair_device`，从 `setup.rs` 搬过来共享。
- 配对复用 `install.ps1` 而不是在 Rust 里重写：令牌生成、icacls 收紧、**重写开机自启项里的 `--device-id`**、重启服务都在那边。漏掉重写自启项，下次开机就会用回旧身份、心跳恒定 401。
- `install.ps1` 新增 `-KeepConsole`：客户端自己调脚本时不能把调用者停掉。

**修掉的 bug：**

| 症状 | 根因 |
| --- | --- |
| 安装必然失败「找不到归档导出程序」 | 远端 `2cdf7ff` 把采集改成只用 `aicrm-export-new.exe` 并移出载荷，忘了改脚本，脚本还在强制找 `AICRM-Export.exe` |
| 「文件正由另一进程使用」 | 脚本只停采集服务，界面/托盘开着时 `Copy-Item` 失败 → 现在停四个进程 |
| 「函数参数列表中缺少 )」 | 删 param 块最后一项时把它前面那项的尾逗号也去掉了，之后插入新参数就语法错误 |
| 点「连接」后客户端直接消失 | `install.ps1` 会停 `aicrm-console.exe`，而调用者正是它 → `-KeepConsole` |

**更正一处旧说法**：配对**不需要**管理员权限，`provisions` 接口只校验登录态。详见上文「安装与配对是两件事」。

### 2026-10-10（退出语义与采集可观测性）

**托盘「退出」= 完全退出。** 以前菜单写的是「退出（不影响服务）」，点了只关界面，
`aicrm_windows_api.exe` 继续在后台占着 8765 —— 用户以为退出了、其实还在跑，
重装或排查时都得先去任务管理器手动杀进程。

- `shell.rs` 菜单文案改为**「退出（同时停止服务）」**，`Action::Quit` 的注释同步更正。
- `console.rs` 新增 `stop_other_client_processes()`：退出前依次结束
  `aicrm_windows_api.exe` / `aicrm-login.exe` / `aicrm-tray.exe`，再退出自己
  （调用者不在列表里，由 `process::exit(0)` 退出）。

**采集过程埋点。** 以前客户端只在**失败**时联系云端，于是卡在导出里（导出程序 hang、
微信没运行、数据目录读不到）时，云端看到的永远是「任务已创建」——
分不清是浏览器没调到本机，还是导出卡死了，只能派人到机器上翻目录。

现在四个节点各上报一次进度（都走云端已有白名单，`progress` 0–75）：

| 时机 | stage | progress | 云端显示 |
| --- | --- | --- | --- |
| 收到采集请求 | `exporting` | 10 | 已收到采集请求，开始导出 … |
| 导出完成、打包完成 | `exporting` | 35 | 导出完成，已打包 N 个会话，准备上传 |
| 开始上传 | `uploading` | 60 | 正在上传归档包（N 字节） |

实现是 `archive_service.rs` 的 `report_task_progress()`，与失败上报同源（由 `upload_url`
推导出 `/status`）。上报失败只打日志，绝不打断采集本身。

**顺带修掉的两个采集 bug**：水位线增量只下发 `--start` 未下发 `--end`，导出程序会判为
参数错误（「`--start` 与 `--end` 必须同时指定」）导致采集当场失败；`today()` 原本按 UTC
取值，改为东八区，避免每天早上八点前把「今天」算成昨天而漏采当天数据。

**同一天早些时候（已单独提交）**：单实例（主界面与安装器共用一把互斥体）、客户端打开即托管本地服务并保活、打包输出名带版本号。

## 自更新（云端控制）

客户端每 20 秒心跳一次上报 `agentVersion`；云端发现落后于已发布版本时下发 `agent.update`。
更新包与指令都**不经过网页** —— 网页只是仪表盘（显示版本落后、提供手动推送按钮）。

- 下载：凭 Device 令牌取 `GET <云端>/api/agent/v1/programs/agent`（整包时逐件再取 `console`；0.1.47 起不再有 `login` 件，0.1.43 前的旧客户端只认单件），本地 SHA-256 校验。下载超时 600 秒 —— 整包里的界面程序近 10 MB，曾用 45 秒长轮询超时，下载必被误杀
- 落盘：`updates/<版本>/` + `updates/pending.json`（**只允许升级写入**：云端换版本快于下载时，后到的旧命令不得覆盖新标记）
- 应用时机（0.1.33 起）：下载校验就绪后**等空闲自动重启应用** —— 重活计数（采集 / 导出 / 绑定）归零、再留 15 秒把命令回执发出去，然后拉起更新器；一直忙则退回「下次启动」兜底路径
- 替换：拉起 **`aicrm-update.exe`**（原生 exe，不经解释器）完成：
  宽限期 → 复算摘要 → 校验目标名白名单 → 备份 → 先停客户端进程（含会保活服务的
  主界面）→ 覆盖（遇占用自动重试）→ 按 `data/state/startup.json` 记录的启动命令重启 →
  清理已下线的旧程序（`aicrm-login` / `aicrm-tray`）→
  刷新桌面/开始菜单快捷方式与图标缓存 → 确认服务存活；失败回滚

三个必须记住的约束：

1. **正在运行的 exe 不能覆盖自己**，替换只能由独立进程做 —— 这也是为什么不用脚本：
   客户机上执行策略或安全软件可能直接拦掉它（开发机复现过：spawn 脚本那一步直接挂住）。
2. **发布产物名（`agent-<版本>.exe`）不是安装名（`aicrm_windows_api.exe`）**。
   目标名固定为安装名，更新器另有目标名白名单兜底：名单外的文件名一律报错，
   而不是悄悄写到别处。
3. **子进程必须重定向 stdio**。子进程若继承父进程管道，父进程退出后写 stdout 会失败，
   而 Rust 的 `println!` 写失败会直接 panic 终止进程 —— 表现为「更新成功、进程却没了」。

## 更新逻辑的权威规格

更新链路的全局设计（版本状态机、完整时序、两端契约表、失败矩阵、并发与去重规则）
见 AICRM 仓库的 support/docs/客户端自更新-全局设计.md。
改客户端或云端的更新相关代码前，先对那张契约表 —— 历史故障全部来自「改了 A 端、没核对 B 端约束」。
