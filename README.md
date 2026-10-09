# AICRM Windows API

作者：Tang。当前 API 只依赖两个辅助程序：Windows 0.4.7 的 `AICRM-Key.exe`（取密钥、列本地账号）与 `aicrm-export-new.exe`（导出 `messages.md`，人工查阅与云端 AI 分析共用同一份 Markdown）。导出程序文件来自用户提供的 `4.1.13.65 -4.1.15.13 聊天记录导出程序`，不修改原辅助程序。原先产 `messages.json` 的 `AICRM-Export.exe` 已不再需要：2026-10 起云端归档与 AI 分析输入都直接用 Markdown，JSON 中转环节整体删除。Rust 服务在**当前用户登录后**由 Windows `HKCU\...\Run` 启动；微信数据和密钥属于当前用户，因此无需以 SYSTEM 身份运行。

## 程序清单

`Cargo.toml` 声明六个二进制（其中 `aicrm-pack` 是打包工具，不随客户端分发）：

| 二进制 | 入口 | 作用 | 用法 |
| --- | --- | --- | --- |
| `aicrm_windows_api` | `src/main.rs` | 后台服务：HTTP 接口 + 云端命令通道 | 无界面，由 `HKCU\...\Run` 隐藏启动 |
| `aicrm-login` | `src/forckend/main.rs` | 登录窗口（egui） | `--endpoint` / `--token-file` / `--device-file`，见下 |
| `aicrm-console` | `src/forckend/console.rs` | 主窗口：侧边栏 + 状态总览（云端 / 密钥 / 服务）+ 配置页 + 账号菜单；**托管本地服务**；**首次未配对时显示「连接云端」引导页** | 无参数；`--hidden` 只进托盘不弹窗；`--start-service` 只起服务不显示窗口 |
| `aicrm-tray` | `src/forckend/tray.rs` | 系统托盘：看服务状态、打开主界面、启停服务、退出 | `--bind` / `--data-dir`；退出时会一并 `taskkill` 掉后台服务 |
| `aicrm-setup` | `src/forckend/setup.rs` | **静默安装**：不问任何东西，打开即装盘 → 装完自动拉起客户端 → 自己退出 | `--endpoint`；`--extract-only` 自检内嵌载荷。单文件形态由 `aicrm-pack` 拼出 |
| `aicrm-pack` | `src/pack/main.rs` | 打包工具：安装器 + 全部载荷 → 单文件安装程序 | `cargo run --release --bin aicrm-pack [输出路径]` |

四件事值得记一笔：

- **外观统一**：`src/forckend/theme.rs` 是三个窗口共用的外观（浅色底、白卡片、描边、留白），各自 `mod theme;` 指向同一份文件，不引入共享库。
- **主窗口不依赖服务**：它直接读安装目录里的文件（令牌、密钥、`cloud.url`、`data/jobs`），因为它的职责之一就是"服务没跑时把服务拉起来"。宽高固定、长文本截断，避免窄窗口把中文挤成竖排。
- **单文件安装程序**：`aicrm-pack` 把 `aicrm-setup.exe` 与载荷 zip 拼在一起，页脚 `[offset u64][len u64]["AICRMSFX"]`；安装器启动时读自身末尾定位并解压载荷，没有页脚就退回"同目录"布局。载荷清单在 `src/pack/main.rs` 顶部。输出名是 `AICRM-<版本号>.exe`，版本号取自 `Cargo.toml`，不用手工同步。
- **一个时间只有一个界面**：`src/forckend/single.rs` 用命名互斥体 `Local\AICRM.WinClient` 做单实例，主界面与安装器**共用同一把锁**，所以两者不会并排出现。重复启动时把已有窗口 `SW_RESTORE` + 置前，而不是再开一个。开窗前先 `acquire()`；安装器装完要拉起主界面前必须 `release()`，否则主界面会被自己那把锁挡掉（表现为"装完了但客户端弹不出来"）。
- **打开客户端 = 服务就该在跑**：主窗口启动时托管 `aicrm_windows_api.exe`，每 5 秒的刷新里检查一次，没起就拉起、掉了就重启。用户手点「停止服务」时置 `auto_service=false` 不再自动拉起，点「启动服务」恢复。服务是采集和心跳的载体，它一掉云端立刻把设备标离线，网页端也就点不动「绑定微信」。

托盘图标颜色就是服务状态（绿=运行中、灰=已停止），菜单里「令牌」一项看 `api.token` 是否非空；「服务在不在」是直接连 `--bind` 端口判定的，比查进程名更能说明"到底能不能用"。

## 安装与运行

在本项目目录运行 `./install.ps1`。脚本编译 Rust 服务，把三个客户端程序（`aicrm_windows_api.exe`、`aicrm-login.exe`、`aicrm-tray.exe`）、`AICRM-Key.exe` 与 Markdown 导出程序放到 `%LOCALAPPDATA%\AICRM-Windows-API\`，生成权限限定为当前用户、SYSTEM 和管理员的 `api.token`，注册登录自启并立即启动。现有密钥仍读取本项目 `src\tools\accounts\`，不会复制或删除。服务默认监听 `127.0.0.1:8765`。需要更换密钥目录或监听地址时使用 `./install.ps1 -KeyDir 'D:\密钥目录' -Bind '127.0.0.1:8765'`。重新运行安装脚本会更新程序并重启服务，不会更换已有 token。

### 设备归属：会话制（2026-10-09 起）

**设备不归账号所有。** 配对（`device.token`）只解决"这台机器是不是真的"，不决定"谁能用它"。后者由云端会话表 `aicrm_agent_device_sessions(device_id, user_id)` 表达，由心跳维护：

```text
客户端登录账号 A → api.token 落盘
        ↓ 下一个心跳（≤20 秒）
PUT /api/agent/v1/status
    Authorization: Device <设备令牌>      ← 证明"我是这台电脑"
    X-AICRM-User-Token: <A 的 JWT>        ← 证明"现在登录的是 A"
        ↓
云端登记 / 刷新 (这台设备, A) 的会话
        ↓
网页端用 A 登录就能看到这台客户端，可以绑定、可以采集
```

由此得到的行为，**完全不需要实施人员介入**：

| 场景 | 结果 |
|---|---|
| 客户端登录 A，网页也登录 A | 网页看得到，可绑定可采集 |
| 客户端换成 B | 下一个心跳登记 B，网页用 B 登录即生效，**不需要重启客户端、不需要重新配对** |
| 网页登录这台机器上从没登录过的 C | 0 台设备，按钮置灰 |

设计要点：

- **不挪任何归属字段。** 早前的"认领制"（把设备记录改挂到某账号）会和按账号隔离的数据打架 —— 设备被挪走，`wechat_accounts`、历史任务还留在旧账号名下，绑定就出错。会话制只在旁边加一行记录，谁的数据都不动。
- **失败不影响心跳。** 令牌过期、账号被禁用只是"这次没登记"，服务照常用。心跳挂了会让整台设备判离线，代价比归属错高得多。
- **切换延迟 20 秒，不需要重启。** 客户端不需要为换账号做任何事，更不需要修改服务端或用认领接口去挪归属记录。

云端侧的实现说明见服务端仓库 `support/docs/device-session-model.md`。

### 安装与配对是两件事

**安装**只负责放文件、注册开机自启，不需要任何账号：

```powershell
./install.ps1 -CloudUrl <云端基地址> [-AllowOrigin <网页域名>]
```

`-ProvisionCode` 可以省略 —— 省略时脚本跳过配对、先落一个占位设备 ID，装完照常可用。真正的配对留给客户端首次打开时的「连接云端」引导页完成（客户端会再调一次本脚本，带 `-DeviceId` / `-ProvisionCode` / `-KeepConsole`）。

**配对不需要管理员权限。** `POST /api/ui/v1/provisions` 只校验登录态与账号是否激活，普通销售账号同样能申领（实测 `test2 / 123456` 返回 201）。以前"装客户端必须给管理员账号"的说法不成立。

`-CloudUrl` 会写进 `%LOCALAPPDATA%\AICRM-Windows-API\cloud.url`，隧道地址变了只改这个文件再重启服务，不用重装。

两个坑：

- 覆盖文件前脚本会停掉 `aicrm_windows_api` / `aicrm-console` / `aicrm-login` / `aicrm-tray`，否则 `Copy-Item` 报「文件正由另一进程使用」。**客户端自己调脚本时必须加 `-KeepConsole`** —— 不然脚本会把调用者停掉，表现为"点了连接、窗口直接消失"。
- 装完脚本会把自身复制一份到安装目录，就是供客户端配对时复用。

采集闭环只依赖 `aicrm-export-new.exe`，它由 `install.ps1 -ExportProgram` 复制进 `tools\`，也是 `Service::new` 的启动校验项，缺它服务起不来。不再依赖 `AICRM-Export.exe`。

### 换云端 / 重装前必读：设备身份有两处

云端认的是 `--device-id`，它出现在**两个地方**，必须一致，否则心跳恒定 `401 Unauthorized`：

1. `%LOCALAPPDATA%\AICRM-Windows-API\device.id` 与 `device.token`
2. **`HKCU Run\AICRM-Windows-API` 自启项里写死的 `--device-id`** ← 最容易漏，改了文件不重装是没用的

而且 `install.ps1` **只在传了 `-CloudUrl` 时**才把 `--cloud-url-file` / `--device-token-file` / `--device-id` 拼进启动参数和自启项。不带 `-CloudUrl` 装出来的服务根本不连云端。

已接入云端时重装**不需要再配对**（`device.token` 存在就会沿用），但**仍要带 `-CloudUrl`**，否则自启项里没有云端参数。确认彻底换云端才删 `device.id` + `device.token` 重新走配对。

验通标准：云端设备列表里这台从 `never_seen` 变成 `offline` / `online`。仍是 `never_seen` 就看 `client.err.log` 有没有 `401 Unauthorized`。

`./uninstall.ps1` 只删除登录自启项并停止服务，保留程序、密钥、token 和导出文件。手动启动命令可查看 `install.ps1` 写入的 `HKCU:\Software\Microsoft\Windows\CurrentVersion\Run\AICRM-Windows-API` 值。

## 登录窗口

`aicrm-login.exe` 是给 Windows 客户端用的图形程序（`src/forckend/`），与后台服务分开：用户在这里输入账号密码，向云端换取**会话令牌**，写进本地令牌文件供后台服务鉴权。**密码完全由云端校验**，本地不保存、不比对 —— 本地校验等于在客户端藏一份密码哈希，可被替换或绕过。

```powershell
./target/release/aicrm-login.exe --endpoint https://你的云端/api/auth/login
```

| 参数 | 默认值 | 说明 |
| --- | --- | --- |
| `--endpoint` | `http://127.0.0.1:8000/api/auth/login` | 云端登录接口，**正式环境必须 HTTPS** |
| `--token-file` | `%LOCALAPPDATA%\AICRM-Windows-API\api.token` | 会话令牌写入位置，与后台服务读取的是同一个文件 |
| `--device-file` | `%LOCALAPPDATA%\AICRM-Windows-API\device.id` | 设备标识，首次运行生成后固定不变 |

令牌文件写好后会尽力用 `icacls` 把权限收紧到当前用户 / SYSTEM / Administrators。**后台服务需重启才会读取新令牌**（服务的令牌是启动时读一次）。

### 云端登录接口契约

客户端发起的这个请求**不带 `Authorization` 头** —— 它就是"用密码换令牌"的入口，不可能要求令牌。代价是它天然是公开端点，防滥用只能靠**强制 HTTPS + 云端失败次数限制**，客户端无能为力。

```http
POST <endpoint>
Content-Type: application/json

{"username":"zhangsan","password":"…","device_id":"3f2a…","client_version":"0.1.0"}
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

客户端要求 `code == 0` **且**响应里带 `data.accessToken`；缺 `data` 或信封形状不对会报「云端返回的不是 {code,message,data} 信封 / 云端返回成功但缺少 data」，那属于前后端版本不匹配，不是凭据问题。`refreshToken` 目前只被解析和打码保留，尚未用于自动续期。客户端把 `4xx` 当作"凭据错误、重试无用"，把 `5xx` 与网络故障当作"可重试"。云端签发令牌后应把它与该 `device_id` 一起入库，**之后云端调用本地 API 时用的就是这同一个令牌** —— 所以令牌不需要任何人工"交付"。`device_id` 让云端能够单独撤销某一台设备而不影响其他设备。

### 本地联调

云端还没就绪时，可用仓库根目录的两个假云端脚本跑通两条链路。

`mock-cloud.ps1` 答登录接口（账号 `demo` / 密码 `123456`，其它组合返回 401）：

```powershell
Start-Process pwsh -ArgumentList '-NoProfile','-File','.\mock-cloud.ps1','-Port','9413','-LogFile',"$env:TEMP\req.txt" -WindowStyle Hidden
./target/debug/aicrm-login.exe --endpoint http://127.0.0.1:9413/api/auth/login --token-file "$env:TEMP\api.token"
```

`req.txt` 会记下客户端实际发出的原始请求，便于核对契约。

`mock-agent-cloud.ps1` 答命令通道，用来在没有真云端时验证「云端下发命令 → 客户端执行 → 回传结果」：

```powershell
Start-Process pwsh -ArgumentList '-NoProfile','-File','.\mock-agent-cloud.ps1','-Port','9414' -WindowStyle Hidden
./target/release/aicrm_windows_api.exe --cloud-url http://127.0.0.1:9414 --device-token dev-token --device-id dev-1 --bind 127.0.0.1:8765
```

它只下发一次 `wechat.export`（`-MaxCommands` 可调），收到结果后退出。它不实现心跳端点 `/api/agent/v1/status`，也不支持 `wechat.bind` / `wechat.collect`，因此服务日志里会出现心跳 404 —— 只影响日志，不影响那条导出命令的往返。

## 接口

除下面单列的握手端点外，所有接口都要求 `Authorization: Bearer <token>`，token 在 `%LOCALAPPDATA%\AICRM-Windows-API\api.token`。接口层只解析请求、调用 `backend/service/` 的函数。该目录按辅助程序拆分：`service/mod.rs` 是共享底座（服务状态、进程调用、并发闸门、`authorize` 鉴权中间件），`service/key_service.rs` 承载 `AICRM-Key.exe` 业务（密钥准备、账号查询），`service/export_service.rs` 承载 `aicrm-export-new.exe` 业务（Markdown 导出执行、结果打包与 NDJSON），`service/archive_service.rs` 承载 Markdown 归档打包与上传（数据采集闭环上半段），`service/agent_service.rs` 是云端命令通道（长轮询 + 心跳）。

鉴权不在各模块内单独实现，而是在 `backend/mod.rs` 的 `app()` 里通过唯一的 `route_layer` 统一施加到全部已注册路由上。新增模块只需写一个 `pub fn router() -> Router<Arc<Service>>` 并在 `app()` 中 `merge` 一行，其所有接口自动受保护，无需写任何鉴权代码；所有 `merge` 都必须写在 `route_layer` 之前。用 `route_layer` 而非 `layer`，是为了让未匹配的路径保持 404 而不是变成 401。`backend/mod.rs` 的测试会遍历表里全部 11 个接口，确认无 token、错 token、其它认证方案均返回 401。

| 方法 | 路径 | 功能 |
| --- | --- | --- |
| GET | `/api/aicrm/key/version` | Key 版本 |
| GET | `/api/aicrm/export/version` | Export 版本 |
| GET | `/api/aicrm/key/help` | Key 命令行帮助 |
| GET | `/api/aicrm/export/help` | Export 命令行帮助 |
| GET | `/api/aicrm/accounts` | 列出本地已保存密钥的 wxid，不返回密钥 |
| POST | `/api/aicrm/prepare` | 验证或捕获密钥；缺少有效密钥时辅助程序可能重启微信 |
| POST | `/api/aicrm/export` | 运行导出，文件保留在本地 `data\jobs\`，返回结果与路径 |
| POST | `/api/aicrm/export/stream` | 运行导出，返回本次每个 `messages.json` 文档的 NDJSON；不含附件字节 |
| POST | `/api/aicrm/export/bundle` | 下载原始 JSON 和附件 ZIP |
| POST | `/api/aicrm/export/ai-bundle` | 下载原始 JSON、附件及精简 `chat.txt` ZIP |
| POST | `/api/aicrm/collect` | 数据采集：后台导出两个程序、合并归档包并上传云端；**立即返回 202，不等导出跑完** |

导出请求示例：`{"wxid":"wxid_xxx","day":14}`。`wxid` 可省略，由辅助程序识别当前账号；`person` 可限定联系人或群名；`day` 为含今天的最近 N 天；`start` 与 `end` 必须同时提供，且不能和 `day` 同用。`task_id`、`wechat_dir` 可选；`export_dir` 是云端指定的落盘目录，给了就直接导到那里（不清理、不删），不给才用 `data\jobs\<id>` 临时目录。请求体带 `deny_unknown_fields`，字段名写错会立刻 422 而不是被静默忽略。未指定联系人和日期即导出全部历史。每个请求使用独立导出目录，所以多个导出可并发；密钥准备操作与导出互斥（同一个 `RwLock` 闸门）。

`/api/aicrm/collect` 的请求体是另一套字段，全部必填：`task_id`、`execution_token`、`user_token`、`upload_url`（形如 `https://云端/api/tasks/<taskId>/upload`），加上与导出同名的可选筛选 `wxid` / `person` / `day` / `start` / `end`。它**不能在请求里等到底**：导出要跑几分钟，挂在请求里的话浏览器一切页面、刷新或关标签就会断开连接、handler 被丢弃，采集被一起取消（归档目录写到一半不动、云端任务永远停在 `created`，实测踩过）。所以它只把活儿丢进后台就回 202，前端照旧轮询 `GET /api/tasks/{id}` 看进度。上传用 multipart，字段名必须是 `file`，并同时带 `X-Execution-Token` 与 `Authorization: Bearer <user_token>` —— 只带执行令牌会被云端 401。

`/stream` 的格式是 Windows 专用 `aicrm-windows-files/v1`：首行 `meta`，每个文件一行 `{"type":"file","path":".../messages.json","document":{...}}`，末行 `summary`。它不伪装成 Android 的 `aicrm-compact/v1`。需要语音、图片等附件时调用 ZIP 接口。

AI ZIP 中 `chat.txt` 按会话和日期分组，保留消息顺序、发送方向及文本。群消息在源数据有成员信息时标注成员。语音只写 `[语音:相对路径]`；原程序未导出音频文件的语音写 `[语音]`。原始 `messages.json` 和附件逐字节保留在 ZIP 对应目录，用于核对。ZIP 或 NDJSON 响应完成或连接中断后，服务会删除本次临时导出文件；`/export` 返回的本地文件则保留，需自行清理。

### 握手端点与 CORS

| 方法 | 路径 | 鉴权 | 功能 |
| --- | --- | --- | --- |
| GET | `/api/aicrm/session` | 无（改为校验 `Origin`） | 把本机令牌交给浏览器里的 AICRM 前端 |

浏览器里的前端读不到 `api.token` 文件（网页没有文件系统权限），所以需要一个握手端点把令牌交给它。它刻意挂在鉴权层之外，但不等于敞开：**只有 `Origin` 在白名单里才返回令牌**，否则 403 `FORBIDDEN_ORIGIN`。浏览器一定会带 `Origin` 且页面脚本改不了它，所以恶意网页拿不到；本机上的其它进程本来就能直接读令牌文件，威胁面没有变大。响应只含 `token` 与 `base_url`，不含任何业务数据。

来源白名单默认是四个本机回环地址（`http://localhost:5000`、`http://127.0.0.1:5000`、`http://localhost:3000`、`http://127.0.0.1:3000`）。**部署到客户机器时必须加**：客户的网页是云端的 HTTPS 域名。用 `--allow-origin <域名>`（可重复）追加，或设环境变量 `AICRM_ALLOWED_ORIGINS`（逗号分隔）；命令行值在环境变量值之后追加去重。`install.ps1` 的 `-AllowOrigin` 就是往自启项里写这个参数。

CORS 中间件包在最外层（比鉴权还外）：预检 `OPTIONS` 不带 `Authorization`，交给鉴权层只会得到 401，所以由中间件直接回 204。响应只在来源被放行时回 `Access-Control-Allow-Origin`（刻意不用 `*`，接口需要凭据），并回 `Access-Control-Allow-Headers: Authorization, Content-Type`、`Access-Control-Max-Age: 600` 与 `access-control-allow-private-network: true` —— 最后这个头是给「公网 HTTPS 页面访问本机 127.0.0.1」用的，Chrome 会先发预检并要求它，不回的话请求在浏览器里就被拦掉，服务端连日志都没有。

### 命令行参数（后台服务）

| 参数 | 默认值 | 说明 |
| --- | --- | --- |
| `--tools-dir` / `--key-dir` / `--data-dir` / `--token-file` | 可执行文件同级的 `tools\` / `accounts\` / `data\` / `api.token` | 目录与令牌位置 |
| `--bind` | `127.0.0.1:8765` | 监听地址 |
| `--allow-origin` | 见上 | 追加网页来源白名单，可重复 |
| `--cloud-url` | 无 | 云端基地址，如 `https://api.example.com` |
| `--cloud-url-file` | 无 | 从文件读云端基地址。**隧道地址会变时用这个**：每轮轮询都重读文件，改文件即可，不用重装 |
| `--device-token` | 无 | 设备令牌明文写在命令行，仅调试用 |
| `--device-token-file` | 无 | 设备令牌文件（推荐：不会出现在进程列表和自启项里）。与 `--device-token` 同给时以文件为准 |
| `--device-id` | 无 | 设备标识 |

### 云端命令通道

客户端装在客户的 Windows 电脑上，位于 NAT 之后、没有公网 IP，云端无法主动连进来，所以方向反过来：客户端定时向云端取命令，执行完回传。**`--device-token*`、`--device-id` 三项齐全，且 `--cloud-url` / `--cloud-url-file` 至少有一个**才启用轮询，否则只跑本地 API。

- 心跳：每 20 秒 `PUT <cloud>/api/agent/v1/status`。云端靠它判断设备在线，没有心跳网页端就不允许对这台设备下发采集任务。`wechat.online` 要求微信进程在跑、wxid 已绑定、本地库在、密钥就绪，任一不成立就如实报 `false`。
  **心跳还顺带把登录账号带给云端**（`X-AICRM-User-Token`，每次现读 `api.token`）—— 这决定了网页端登录哪个账号才看得见这台客户端，详见下方「设备归属：会话制」。
- 取命令：`POST <cloud>/api/agent/v1/commands/poll` 长轮询，挂起 `waitSeconds=25` 秒。
- 回传：`PUT <cloud>/api/agent/v1/commands/<id>/result`。
- 命令类型三种：`wechat.bind`（取密钥，可能重启微信，云端只应在确认客户在场时下发）、`wechat.export`（导出，结果留在本机）、`wechat.collect`（导出 + 归档上传，结果交回云端）。不支持的类型回 `UNSUPPORTED_COMMAND`，不会中断轮询。
- 这三个请求都用 `Authorization: Device <token>` + `X-AICRM-Device-ID` 两个头，与本地 API 的 `Bearer` 不是同一套。

## 本机调用

```powershell
$base = 'http://127.0.0.1:8765'
$token = [IO.File]::ReadAllText((Join-Path $env:LOCALAPPDATA 'AICRM-Windows-API\api.token')).Trim()
$headers = @{ Authorization = "Bearer $token" }
Invoke-RestMethod "$base/api/aicrm/accounts" -Headers $headers

$body = @{ day = 14 } | ConvertTo-Json -Compress
Invoke-WebRequest "$base/api/aicrm/export/ai-bundle" -Method Post -Headers $headers `
  -ContentType 'application/json; charset=utf-8' `
  -Body ([Text.Encoding]::UTF8.GetBytes($body)) `
  -OutFile "$env:USERPROFILE\Desktop\测试\windows-ai.zip"
```

2026-09-29 本机真实接口测试：两个导出请求同时执行成功。14 天全联系人 AI ZIP 中有 120 个原始聊天 JSON、2,527 条消息、35 条语音消息，其中 30 个有音频文件、5 个源程序未导出文件。`chat.txt` 有 2,527 条消息行、74,413 字符，无 `<voicemsg>` XML；所有 30 个语音文件引用均可在 ZIP 中找到。NDJSON 有 122 行（meta + 120 文件 + summary），消息数一致。传输后临时任务目录为空。测试产物在 `C:\Users\11589\Desktop\测试\AICRM_Windows_API_20260929\`。服务启动、Bearer 鉴权（无 token 返回 401）及登录自启注册已核对；实际注销后重新登录的自动启动尚未测试。

随后单独复测最终版 NDJSON，源数据增加至 2,541 条消息，末行 summary 成功且不含已删除的临时路径。密钥接口用 `no_restart=true` 验证成功，返回值不含密钥正文；单联系人原始 ZIP 包含 5 个 JSON 和 2 个音频文件；`/export` 的本地文件模式返回 185 条消息、5 个 JSON，结果保留在其 `job_dir`。两个帮助接口均返回 200。

## 更新记录

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

客户端每 20 秒心跳一次上报 ；云端发现落后于已发布版本时下发 。
更新包与指令都不经过网页 —— 网页只是仪表盘。

- 下载：凭 Device 令牌取 ，本地 SHA-256 校验
- 落盘： + 
- 替换：拉起 ****（原生 exe，不经解释器）完成
  宽限期 → 复算摘要 → 备份 → 停进程 → 覆盖 → 按  重启 →
  刷新桌面/开始菜单快捷方式与图标缓存 → 确认服务存活；失败回滚

三个必须记住的约束：

1. **正在运行的 exe 不能覆盖自己**，替换只能由独立进程做；
2. **发布产物名（）不是安装名（）**。
   目标名固定为安装名，更新器另有目标名白名单兜底；
3. **子进程必须重定向 stdio**。继承父进程管道会在父进程退出后写失败，
   Rust 的  写失败会 panic 终止进程 —— 表现为「更新成功、进程却没了」。
