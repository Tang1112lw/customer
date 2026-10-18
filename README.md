# AICRM Windows API

> **2026-10-10 变更（重要）**：**设备绑定 / 配对已彻底删除**。没有 `device.id` /
> `device.token` / 预置码，也没有设备表与会话表；云端只认「客户端有没有登录与云端
> 相同的账号」。登录同一账号即可直接控制客户端；一个账号只能登录一个客户端。
> 本文档下方仍可见「设备」「配对」字样的段落属**历史记录**（更新记录一节保留原样），
> 当前行为以《`src/support/项目架构.md`》与本节为准。
> 另外：安装器现在会先做**环境准备**（系统版本 / 磁盘 / 权限 / 运行库 / 云端连通性），
> 全部必须项通过才允许安装；我们自己的程序已改为**静态链接 CRT**，客户机不需要预装 VC++。
>
> **0.1.48 补充**：网页点「绑定微信账号」= **一律真抓**（客户端不再复用本机已有密钥，
> 云端绑定指令一律带 `force`）；客户端会自己定位微信程序（注册表 + 常见目录），
> 找不到时给可照做的中文提示，而不是把工具原话抛给客户。

作者：Tang。当前 API 只依赖两个辅助程序：Windows 0.4.7 的 `AICRM-Key.exe`（取密钥、列本地账号）与 `aicrm-export-new.exe`（导出 `messages.md`，人工查阅与云端 AI 分析共用同一份 Markdown）。导出程序文件来自用户提供的 `4.1.13.65 -4.1.15.13 聊天记录导出程序`，不修改原辅助程序。原先产 `messages.json` 的 `AICRM-Export.exe` 已不再需要：2026-10 起云端归档与 AI 分析输入都直接用 Markdown，JSON 中转环节整体删除。Rust 服务在**当前用户登录后**由 Windows `HKCU\...\Run` 启动；微信数据和密钥属于当前用户，因此无需以 SYSTEM 身份运行。

## 程序清单

`Cargo.toml` 声明五个二进制（其中 `aicrm-pack` 是打包工具，不随客户端分发）：

| 二进制 | 入口 | 作用 | 用法 |
| --- | --- | --- | --- |
| `aicrm_windows_api` | `src/main.rs` | 后台服务：HTTP 接口 + 云端命令通道。参数全部可选，默认从 exe 同目录读配置 | 无界面，由 `HKCU\...\Run` 隐藏启动 |
| `aicrm-console` | `src/forckend/console.rs` | 主窗口：主界面只剩「最近采集」记录；右上角显示绑定状态（已绑定 / 未绑定 + 微信号），点它展开账号菜单（重新登录 / 退出登录）；「配置」页入口已隐藏（页面代码保留，改云端地址走重装或直接编辑安装目录下的 `cloud.url`）；**登录 / 重新登录都在这一窗口内完成**（登录引导页为玻璃质感 + 左侧浅绿玻璃品牌栏）；**托管本地服务**；托盘形态由 `--hidden` 提供（原独立托盘程序已并入） | 无参数；`--hidden` 只进托盘不弹窗；`--start-service` 只起服务不显示窗口 |
| `aicrm-update` | `src/update/main.rs` | **原生更新器**（取代 PowerShell 脚本）：校验摘要与目标名白名单 → 先停客户端进程 → 备份 → 覆盖 → 按 `startup.json` 重启 → 探活失败回滚；也承担快捷方式创建（`--ensure-shortcut`）与旧程序清理 | 由服务在应用更新时拉起；`--ensure-shortcut` 供安装脚本调用 |
| `aicrm-setup` | `src/forckend/setup.rs` | **静默安装**：不问任何东西，打开即装盘 → 装完自动拉起客户端 → 自己退出。窗口 560×480（与登录窗同尺寸）、玻璃质感；环境检测以进度条呈现（检测中扫描动画 / 通过满格绿条 / 未通过给出第一条必须项原因，明细写错误日志）。OpenGL 已随包（软件渲染兜底），任何机器都开得出窗口；窗口创建失败会明确报错，不再降级无界面安装（2026-10-13 删除该路径） | `--endpoint`；`--extract-only` 自检内嵌载荷；`--check` 无界面环境自检（给支持人员，不安装）。单文件形态由 `aicrm-pack` 拼出 |
| `aicrm-pack` | `src/pack/main.rs` | 打包工具：安装器 + 全部载荷 → 单文件安装程序 | `cargo run --release --bin aicrm-pack [输出路径]` |

（`aicrm-login.exe` 登录窗口已于 2026-10-10 并入主控台，不再分发。）

四件事值得记一笔：

- **外观统一**：`src/forckend/theme.rs` 是两个窗口（主控台、安装器）共用的外观（浅色底、白卡片、描边、留白），各自 `mod theme;` 指向同一份文件，不引入共享库。跨二进制共享的无界面工具统一在 `src/shared/`（路径约定 / 设备凭据 / 进程管理 / 字体 / 缓存探测 / 机主昵称 / base64 / sha256），各二进制按需引入。
- **主窗口不依赖服务**：它直接读安装目录里的文件（令牌、密钥、`cloud.url`、`data/jobs`），因为它的职责之一就是"服务没跑时把服务拉起来"。宽高固定、长文本截断，避免窄窗口把中文挤成竖排。
- **单文件安装程序**：`aicrm-pack` 把 `aicrm-setup.exe` 与载荷 zip 拼在一起，页脚 `[offset u64][len u64]["AICRMSFX"]`；安装器启动时读自身末尾定位并解压载荷，没有页脚就退回"同目录"布局。载荷清单在 `src/pack/main.rs` 顶部。输出名是 `AICRM-<版本号>.exe`，版本号取自 `Cargo.toml`，不用手工同步。
- **一个时间只有一个界面**：`src/forckend/single.rs` 用命名互斥体 `Local\AICRM.WinClient` 做单实例，主界面与安装器**共用同一把锁**，所以两者不会并排出现。重复启动时把已有窗口 `SW_RESTORE` + 置前，而不是再开一个。开窗前先 `acquire()`；安装器装完要拉起主界面前必须 `release()`，否则主界面会被自己那把锁挡掉（表现为"装完了但客户端弹不出来"）。
- **打开客户端 = 服务就该在跑**：主窗口启动时托管 `aicrm_windows_api.exe`，每 5 秒的刷新里检查一次，没起就拉起、掉了就重启。主界面的启停按钮撤下后（2026-10-13），入口只剩托盘菜单：托盘「停止服务」置 `auto_service=false` 不再自动拉起，托盘「启动服务」置回 `true` 恢复托管。服务是采集和心跳的载体，它一掉云端立刻把设备标离线，网页端也就点不动「绑定微信」。

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

## 绑定微信：网页点按钮 → 客户端**真抓**

网页上点「绑定微信账号」的语义是：**让那台电脑重新抓一次密钥**。这里有三层"复用"，
改动时必须一起看：

| 层 | 谁会"复用" | 现在的做法 |
| --- | --- | --- |
| ① 客户端包装层 `Service::prepare` | 我们自己 | **不复用**：只保留服务每 10 分钟自检用的 `no_restart`（那条绝不能重启客户微信） |
| ② 抓密钥程序 `AICRM-Key.exe` | 第三方自带的行为：手册原文「验证通过会直接显示"复用，未重新抓取"，不会重启微信」 | 用 `--force` 压过它（手册原文：「忽略已有密钥并重新捕获」） |
| ③ 云端绑定指令 | 控制面 `CreateBinding` | 一律带 `force: true` |

于是行为是确定的：点一次绑定 = 一定跑一次真抓（微信会重启、需要重新登录，网页弹窗
已经这么提示）；本机已有密钥也照抓；抓不到就报真实原因，**不会拿旧密钥冒充成功**。

**微信程序路径由客户端自己找**（`src/shared/wechat.rs`）：注册表安装登记
（`...\Uninstall\Weixin` 的 `DisplayIcon` / `InstallLocation`，装在 D 盘也能查到）→
常见安装目录（`%ProgramFiles%\Tencent\Weixin`、`...\WeChat` 等）。找到就把完整路径
作为 `--weixin-exe` 传给抓密钥程序；找不到时给的是"请确认微信已安装并已登录"这种
能照做的提示，而不是把工具那句「请用 --weixin-exe 指定」甩给客户。

## 接口

除下面单列的握手端点外，所有接口都要求 `Authorization: Bearer <token>`，token 在 `%LOCALAPPDATA%\AICRM-Windows-API\api.token`。接口层只解析请求、调用 `backend/service/` 的函数。该目录按辅助程序拆分：`service/mod.rs` 是共享底座（服务状态、进程调用、并发闸门、`authorize` 鉴权中间件），`service/key_service.rs` 承载 `AICRM-Key.exe` 业务（密钥准备、账号查询），`service/export_service.rs` 承载 `aicrm-export-new.exe` 业务（Markdown 导出执行），`service/archive_service.rs` 承载 Markdown 归档打包与上传（数据采集闭环上半段），`service/agent_service.rs` 是云端命令通道（长轮询 + 心跳）。跨二进制共享的无界面工具在 `src/shared/`（路径约定 / 设备凭据 / 进程管理 / 字体 / 缓存探测 / 机主昵称 / base64 / sha256）。

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

### 2026-10-15（安装器：任何机器都装得上 · 归档带当天媒体预算）

**背景**：客户（开发机）双击安装程序再次看到「AICRM 安装程序启动失败：
egui_glow: OpenGL: egui_glow requires opengl 2.0+」，一个文件都没装上。
10-12 已随包带了 Mesa 软件 OpenGL、10-13 又删掉了降级兜底，结果**安装器自己**
成了那台开不出窗口的机器 —— 单文件 GUI 的旁挂 DLL 到不了它，删掉兜底等于删掉
安装能力本身。

**改法**：

- **安装器自带软件 OpenGL**（`setup.rs`）：`opengl32.dll` 是编译期导入表依赖，
  进程一启动就由 loader 按「应用目录优先」解析，运行期 `SetDllDirectory` 来不及。
  改为启动时把载荷里的 5 个 Mesa DLL 和**自身副本**落进同一个临时目录，从副本重启
  （带 `AICRM_SWGL_STAGED` 防循环 + `GALLIUM_DRIVER=llvmpipe`）—— loader 于是优先
  加载 Mesa，安装窗口在只有 OpenGL 1.1 的机器上也能开。
- **只走有界面一条路，不再留无界面安装兜底**：随包 Mesa 已保证任何机器都开得出
  窗口，开不出就是明确故障 —— 报错退出并写日志，不静默降级。客户看不到安装过程
  比装不上更糟（2026-10-15 用户明确要求：必须使用 OpenGL）。
- **安装全程持单实例锁**：曾「开装即放锁」，重复双击会并发跑两个 `install.ps1`，
  并发写同一个安装目录 —— 安装就这样莫名其妙失败。锁持到装完，`install.ps1`
  末尾不再自己拉起客户端（新增 `-LaunchConsole`，安装器传 false，由它统一拉）。
- **拉起客户端移出 UI 线程**：`open_console` 里「等 1.5 秒看活没活」的重试原来跑在
  `poll()`（界面线程），安装窗冻结至多 3 秒；整个挪进安装线程，界面只等一条消息。
- **归档当天媒体总量上限 48MB**（`archive_service.rs`）：单文件 16MB 上限挡不住
  「一天攒很多」—— 带图片视频的日子 Base64 后足以顶穿云端 128MB 请求体上限，
  把**整包**归档带走。到顶当天收手，文字照发。
- **`CREATE_NO_WINDOW` 收敛**：从 6 处副本收敛为 `shared/process.rs` 的 `hidden()`
  一处（credentials 按「共享文件零交叉引用」约定保留自有副本；`service/mod.rs`
  用的是 tokio Command，另一个类型，保留并注明原因）。
- 文档同步：`项目架构.md` 改口（`--silent` 已删、降级路径恢复、新增安装器
  OpenGL 小节）；归档 v3 的 media 双层上限写进注释与测试。

### 2026-10-16（审查轮：安装器惰性重启 · 归档整包媒体预算 · 死代码清理）

对 10-15/10-16 凌晨的改动做一轮两轴审查（规范 + 规格），修掉的问题：

- **安装器改成惰性重启**：此前每次启动都无条件复制上百兆 Mesa + 自身副本再重启，
  硬件 GL 完好的机器（绝大多数）白付一次复制与重启。现在先直接开窗口，只有
  `run_native` 失败（只剩 OpenGL 1.1 的远程桌面会话、驱动没装全）才带 Mesa 重启
  一次。配套修了一个会把这个优化变成事故的坑：**重启前必须放掉单实例锁** ——
  `run()` 失败前已经拿过锁，不还回去，子进程开界面时会被"已有实例"直接挡掉
  （正是以前修过的"装完了客户端弹不出来"）。
- **窗口创建失败退出码 1**：此前报完弹窗后进程静默退 0，脚本与支持人员无法区分
  "装完了"与"起不来"。
- **open_console 不再谎报成功**：二次重试后客户端仍秒退时，此前返回
  "已启动客户端" —— 界面显示"安装完成"而客户端根本没起来。现在明确报错并给出
  手动双击的指引。
- **归档第三道媒体闸：整包 64MB**：原有单文件 16MB + 单天 48MB 两道闸管不住
  "很多天" —— 一次采集覆盖十来天就是近五百兆，Base64 后远超云端请求体上限
  （128MB），整包上传失败、十多天文字一起丢。到顶后剩余媒体跳过（文字照发），
  带回归测试（跨天累计，不断言按天分布 —— 扫描顺序看文件系统心情，但总量恒定）。
- **删除死代码**：`cloud.rs` 的 `transport_warning`（登录页精简提示后无人调用）、
  `theme.rs` 的 `WARN`（注释说安装器在用，实际两处都不用）、安装器里的
  `mod cloud;`（整个模块在安装器里零引用）。
- **install.ps1 trap 退出码改 2**：与 CheckOnly「有必须项没过」的 1 区分，
  支持人员手工跑时看退出码就知道该看结论还是该看 ERROR 行。

**实测**：打成单文件安装包（71.5MB）—— `--extract-only` 14 个文件完整（含 Mesa
5 DLL）；`install.ps1 -CheckOnly` 退出码 0、6 项检查全过、stdout 按字节确认是
正确 UTF-8（中文无乱码）；GUI 直开路径窗口正常；**惰性重启路径**用「Mesa 放副本旁
+ 故意设坏 `GALLIUM_DRIVER`」强制首次开窗失败复现：父进程（桌面副本）开窗失败 →
放锁 → staging（`aicrm-swgl-<pid>`，5 DLL + exe 副本 6 个文件）→ 子进程从 staging
启动、窗口正常、单实例锁正常拿到（证明放锁生效）、环境检查 6/6 通过。云端侧
`aicrm-cloud` 已用合并后的源码重新构建部署，健康检查通过。

### 2026-10-16（凌晨：安装器 staging 收尾三件套）

对 10-15 的「自带 Mesa 重启自己」做一轮审查，实测复现了三个收尾问题，逐一修掉：

- **装完删的是客户原本双击的那个安装包**：staging 重启后，子进程的
  `current_exe()` 是临时目录里的副本，原来的自删逻辑删的是副本 —— 客户桌面上的
  安装包原封不动，与「桌面只该有一个客户端图标」的意图相悖。父进程现在把原始路径
  经 `AICRM_SETUP_ORIGIN` 透传给子进程，收尾删它；临时副本由父进程连 staging
  目录一起收走。
- **staging 目录纳入陈旧清理**：父进程被强杀 / 死机 / 重启时，
  `aicrm-swgl-<pid>`（软件 OpenGL + 安装程序副本，约 130MB）会永久留在临时目录
  —— 实测复现：一次调试中断就留了一坨，一个半小时后还在。`remove_stale_payload_dirs`
  现在连 `aicrm-swgl-*` 一起清，正在运行的那个（`AICRM_SWGL_STAGE` 指明的）除外。
- **staging 失败留痕**：此前任何一步失败都静默退回原进程，1.1 机器上随后只看到
  OpenGL 报错、日志里查不到原因。现在失败原因写进 `install.err.log`
  （「软件 OpenGL 就位失败」）；没有载荷（开发机直接跑 cargo 产物）不算故障，不写。
- **顺手**：`--check` / `--extract-only` 不建窗口，跳过 staging（支持工具不必先复制
  上百兆再重启一遍）；防重入标记与 staging 路径合并为一个 `AICRM_SWGL_STAGE`；
  窗口创建失败的报错文案改成可执行版本（提示就位失败的常见原因）。

**实测**（本机，硬件 GL 完好，靠环境变量强制 llvmpipe）：打成单文件安装包跑 GUI 路径
—— 子进程从 `%TEMP%\aicrm-swgl-<pid>\` 启动、窗口正常创建、进程加载的
`OPENGL32.dll` / `libgallium_wgl.dll` 确认来自该目录而非 System32；强杀父子进程后
残留 128MB staging 目录，下一次运行被陈旧清理自动收掉；`--extract-only` 退出码 0、
不再触发 staging。

### 2026-10-12（安装器：窗口起不来也能装）

**背景**：客户机（Win10）双击安装程序报
`egui_glow: OpenGL: egui_glow requires opengl 2.0+`，随即退出，一个文件都没装上。

**根因**：安装器是 eframe/egui（glow 后端），要求显卡提供 OpenGL 2.0+；那台机器只给出 **1.1**。
要么显卡驱动没装全（「Microsoft 基本显示适配器」只有 1.1 的软件实现），
要么实施人员是**通过「远程桌面」**连过去操作的（RDP 会把 OpenGL 强制降到 1.1，与驱动无关）。
窗口在**创建阶段**就失败，而这一步在 `install.ps1` 之前 —— 所以安装根本没开始。

**改法**（`src/forckend/setup.rs`、`src/shared/dialog.rs`）：

- **窗口起不来就换一条路走，而不是放弃**：`run_native` 返回 `Err` 时自动降级为无界面安装 ——
  解载荷 → 跑 `install.ps1` → 原生 `MessageBoxW` 报结果 → 删安装包自己 → 拉起客户端。
  安装动作本来就不需要窗口，「能不能画窗口」不该决定「能不能安装」。
- `install_payload(base, prepared)`：窗口路径与无界面路径**共用一处安装实现**
  （此前这段逻辑内联在 `SetupApp` 的安装线程里），不会出现「改了 GUI 那条、忘了无界面那条」。
- 反馈只用 Win32 原生提示框（不依赖 OpenGL）—— 这类机器上它是唯一还弹得出来的东西。
- 成功提示按 `SESSIONNAME` 归因：`RDP-` 开头 → 提示**在这台电脑上重跑一次**
  （远程桌面下界面永远起不来）；否则 → 提示**补装显卡驱动**。
  这两种情况用户该做的事正好相反，所以必须分开说。
- 新增 `--silent` / `--no-gui`：实施人员可显式指定无界面安装（远程 / 无人值守）。

**顺带修掉一个从来没生效过的收尾动作**：`remove_self_after_exit()`（装完删掉安装包自己）
一直没成功过。`Command::args(["/c", script])` 会给含空格的参数自动加外层引号、并把内部的
`"` 转义成 `\"`，而 `cmd.exe` 不认这种转义 —— `del` 拿到的是坏路径；错误又被 `let _ =`
丢弃，于是静默失败。改用 `CommandExt::raw_arg`（原样传递、不做转义）后实测生效：
修复前安装包原地不动，修复后延迟几秒即被删除。

**试过又放弃的方案**：先写了一个配套的 PowerShell 脚本（`--extract-only` + `install.ps1`）。
两条理由否掉：客户不会执行 PowerShell；更要紧的是客户机常被组策略限制脚本执行，
`-ExecutionPolicy Bypass` 会被组策略盖掉 —— 这与当初把更新器做成原生 exe、不做脚本是同一个原因。

**当时没解决的部分**：装完之后主控台界面在这类机器上仍然打不开（同一个 OpenGL 问题）。
当天下午用「随包带一份软件 OpenGL」补上了，见下一节。

### 2026-10-12（下午：软件 OpenGL —— 任何机器都开得出窗口；自启不再拦安装）

接上一节：那条「没解决的部分」——主控台窗口在只有 OpenGL 1.1 的机器上（**远程桌面**，或
**显卡驱动没装全**）打不开 —— 当天下午解决了。同一个下午还修掉了另一颗雷：客户机的
「开机自启项」写不进去时，安装被当成**必须项**拦掉，整台机器一个文件都装不上。

#### 一、软件 OpenGL：随包带一份 Mesa（llvmpipe）

**思路**：界面框架要 OpenGL 2.0+，而机器只给 1.1 —— 那就别跟显卡要，**把渲染搬到 CPU**。
随包带一份纯软件的 OpenGL（Mesa 的 llvmpipe 后端），装进安装目录，Windows 会优先用它们
（应用本地装配，与 `vcruntime140.dll` 同一套做法）。

| 位置 | 改动 |
| --- | --- |
| `src/tools/swgl/` | **新增**：`opengl32.dll`、`libgallium_wgl.dll`、`msvcp140.dll`、`vcruntime140.dll`、`vcruntime140_1.dll`（Mesa 26.2.4 / llvmpipe，x64） |
| `src/pack/main.rs` | 这 5 个文件进载荷的 `tools/swgl/`；载荷 40MB → 64MB，单文件安装程序 47.8MB → 71.9MB |
| `install.ps1` | 铺到**安装目录**（也就是 `aicrm-console.exe` 旁边）。缺文件不拦安装 |
| `src/forckend/console.rs` | 进程起来第一件事就置 `GALLIUM_DRIVER=llvmpipe` + `LIBGL_ALWAYS_SOFTWARE=1` |

**最后那条是必须的，不是保险**：实测少设这两个变量时，Mesa 会去找硬件驱动并**当场失败**
（进程秒退、无日志）；设上之后窗口正常创建。

**实测**：把新的 `aicrm-console.exe` 与这 5 个 DLL 放在同一个目录，**不设任何外部环境变量**
启动 —— 窗口正常创建（标题 `AICRM`），并且核对过进程加载的 `OPENGL32.dll` /
`libgallium_wgl.dll` 确实来自该目录，而不是 `C:\Windows\System32`。

**代价与边界**：

- 客户端界面改走 **CPU 软件渲染**。状态窗口只在画面变化时重绘，实际基本无感；换来的是
  远程桌面 / 无驱动机器上窗口一定能开。
- 包体 **+24MB**（就是 `libgallium_wgl.dll` 那一个 61.8MB 的文件）。
- **安装器本身仍是单文件 GUI**，在只有 1.1 的机器上照旧走「无界面安装」（该降级 2026-10-15 已按用户要求移除，安装器改为自带 Mesa 重启自己，见上文；它没法给自己旁挂
  DLL）—— 但装完之后客户端窗口一定能开，这正是客户要的结果。

**构建前提**：`src/tools/swgl/` 沿用「载荷二进制不进 Git」的规矩（同 `AICRM-Key.exe`）。
从干净仓库构建安装程序前，要先把这 5 个文件放到位，否则 `aicrm-pack` 会直接报「读不到」。
来源：`mesa-dist-win` 的 `mesa3d-<版本>-release-msvc.7z`，取其中的 `x64\` 那几个文件；
`msvcp140.dll` / `vcruntime140*.dll` 取自本机 `System32`（Mesa 的 DLL 是 MSVC 动态链接的）。

#### 二、开机自启：从「必须项」降为「提醒项」，并加启动文件夹兜底

**背景**：客户的 Win10 机器上，环境准备第 4 项 `开机自启可注册` 失败，报
`尝试执行未经授权的操作`（写 `HKCU\...\Run` 被拒 —— 安全软件锁启动项 / 域策略 / 受限账户
都可能）。这一项当时是 `required`，脚本 `throw`，**整个安装当场终止**。

**改法**（`install.ps1`）：

- 该检查从 `required` 降为 **`advisory`**：写不进自启项只影响「开机自动起来」，客户手动双击
  桌面图标照样能用（主窗口打开时会自己把后台服务拉起来），不该拦安装。
- **两级兜底**：先试 `HKCU Run`；写不进就退到**启动文件夹快捷方式**
  （`%APPDATA%\Microsoft\Windows\Start Menu\Programs\Startup\`，放 `AICRM.lnk` 与
  `AICRM-Windows-API.lnk`）——启动文件夹基本不受「启动项」策略和安全软件影响。
- **结尾那两处 `Set-ItemProperty` 也包进了 `try/catch`**。这点容易漏：只改检查不改这里，
  脚本跑到底还是会在同一处抛错，而开头是 `$ErrorActionPreference='Stop'`，安装照样断。
- 收尾如实报告自启落在哪里（`HKCU Run` / `启动文件夹（路径）` / 未注册），不再假装成功。

**实测**：本机跑 `install.ps1 -CheckOnly` 输出 `CHECK|advisory|开机自启|pass|HKCU Run`、退出码 0；
再故意把 `$runKey` 指向一个写不进去的位置，`Get-AutostartKind` 返回 `startup-folder` 并按预期
生成了两条快捷方式。

### 2026-10-13（下午：安装/登录界面玻璃化 · 检测改进度条 · 去提示文字）

按客户逐轮预览选定方案实施（预览图与需求记录见 `项目/AICRM客户端精简/`）：

- **玻璃质感**：egui 无背景模糊，玻璃感 = 浅灰绿底 × 半透明白层（82%）× 顶部 2px 受光高光边；`theme.rs` 新增 `glass_card` / `glass_side` / `glass_input` / `glass_progress` / `check_badge` / `glass_button` 一套 helper。侧栏是**整幅纯色**薄荷绿，不叠光泽层 —— 叠层硬边会在纯色上切出明暗带（客户反馈的"色彩断层"）。
- **安装界面**（方案 C，Hero 居中）：窗口 520×546 → **560×480**（与登录窗同尺寸）；七项检查清单 → **进度条**（检测中扫描动画、通过满格绿条、未通过红条 + 第一条必须项原因，明细写错误日志）；48px 绿色对勾徽标；两个按钮**同宽同色对齐**（复用 `glass_button` 同一表单，仅宽窄/文字不同）；删除副标题、HTTPS 明文警告条等一切非必要提示文字。
- **登录引导页**：重排为左侧浅绿玻璃品牌栏（AICRM 深绿字）+ 右侧玻璃表单卡；输入框改玻璃框；删除重复的「登录」标题与品牌图标，只留 AICRM 文字与按钮文字。
- **删除无界面安装降级路径**：OpenGL 随包后开不出窗口就是明确故障，`install_without_window` / `--silent` / `--no-gui` / run_native 失败降级全部移除，失败时原生提示框报清原因；`--check` 无界面自检保留（支持诊断用）。
- **顺带修复**：托盘「停止服务」补 `auto_service=false` 置位（此前停止后会被下一轮 5 秒刷新自动拉起，停止形同没停）；安装开始即放掉单实例锁（此前 install.ps1 装完自拉客户端会被安装器握着的手锁挡死，表现为"装完了客户端弹不出来"）。

### 2026-10-13（主界面精简 · 全链路藏窗 · 窗口改小）

按客户要求的一轮界面与部署整改（对应 `项目/AICRM客户端精简/需求文档.md`）：

- **CMD 窗口全链路根治**：安装（`setup.rs` 的 powershell）、登录写令牌（`credentials.rs` 的 icacls）、心跳探测（`agent_service.rs` 的 tasklist）、辅助程序（`service/mod.rs` 的 `hidden()`）、更新探测（`shared/process.rs`）一律加 `CREATE_NO_WINDOW`；开机自启路径的服务控制台窗口由 `main.rs` 的 `hide_own_console()` 自藏（仅 release，debug 保留窗口看输出）。
- **主界面只剩「最近采集」**：撤下云端连接卡片、密钥获取卡片（含「打开密钥目录」）、本地服务卡片，以及整套云端可达性探针（`spawn_probe` 等）；`Snapshot` 删掉只写不读的 `token` / `credential_ready` 字段。
- **右上角改绑定状态胶囊**：只显示「已绑定 / 未绑定」，已绑定带微信号；整块即账号菜单入口（重新登录 / 退出登录，弹窗同步做小：148 → 112）。
- **「配置」入口隐藏**：侧栏注释掉该入口，页面代码与内容原样保留（`#[allow(dead_code)]`）；改云端地址走重装或直接编辑安装目录下的 `cloud.url`。
- **窗口改小**：780×604 → 560×480；登录页同步精简为「账号 / 密码 / 登录」。
- **随包软件 OpenGL**（上一提交已带）： Mesa llvmpipe 的 5 个 DLL 随安装包装载，OpenGL 1.1 / 远程桌面等无硬件加速的客户环境也能开得出窗口。
- 终审修复：托盘「停止 / 启动服务」补上 `auto_service` 置位（此前托盘停止后会被下一轮 5 秒刷新自动拉起，停止形同没停）；`binding()` 的 local_only 分支不再把导出昵称错配到首个就绪账号；顶栏显示名超长截断防溢出。

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

- 下载：凭**登录令牌**（`Authorization: Bearer <api.token>`）取 `GET <云端>/api/agent/v1/programs/agent`（整包时逐件再取 `console`；0.1.47 起不再有 `login` 件，0.1.43 前的旧客户端只认单件），本地 SHA-256 校验。下载超时 600 秒 —— 整包里的界面程序近 10 MB，曾用 45 秒长轮询超时，下载必被误杀
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

### 发布新版本（服务端）

```bash
cd windows/control-plane
go run ./cmd/publish-program -kind agent   -version 0.1.48 -file agent-0.1.48.exe
go run ./cmd/publish-program -kind console -version 0.1.48 -file console-0.1.48.exe
sudo systemctl restart aicrm-control-plane   # programs.json 在启动时读入
```

**老客户端升不动的坑**：0.1.47 及更早的客户端讲的是设备令牌协议，与新云端不兼容
（心跳 / 取命令都会 401）—— 它们**收不到更新命令**，必须用新安装包覆盖一次；
覆盖之后即可自动更新。2026-10-10 实测：本机 0.1.47 → 0.1.48 全自动完成
（服务端日志 `已下发客户端更新 from 0.1.47 to 0.1.48`）。

## 更新逻辑的权威规格

更新链路的全局设计（版本状态机、完整时序、两端契约表、失败矩阵、并发与去重规则）
见 AICRM 仓库的 support/docs/客户端自更新-全局设计.md。
改客户端或云端的更新相关代码前，先对那张契约表 —— 历史故障全部来自「改了 A 端、没核对 B 端约束」。
