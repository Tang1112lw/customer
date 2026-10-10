param(
    [string]$InstallDir = (Join-Path $env:LOCALAPPDATA 'AICRM-Windows-API'),
    # 密钥目录。默认放在安装目录下 —— 部署到别的机器时密钥应由本机自行采集，
    # 不能沿用开发机的项目路径。开发机想继续用既有密钥就显式传 -KeyDir。
    [string]$KeyDir = (Join-Path $InstallDir 'accounts'),
    [string]$Bind = '127.0.0.1:8765',

    # ---- 命令通道（云端控制本机取密钥/导出）----
    # 云端基地址，例如 https://aicrm.example.com；给了才启用命令通道。
    [string]$CloudUrl = '',
    # 允许调用本机客户端接口的网页来源（客户的云端域名）。可重复传，或逗号分隔。
    [string]$AllowOrigin = '',

    # ---- 辅助程序来源 ----
    # 留空则自动判定：开发机用 src\tools，部署包用脚本同级的 tools\。
    [string]$ToolSource = '',
    # Markdown 导出程序。默认在 ToolSource 里找；找不到就报错，不静默降级。
    [string]$ExportProgram = '',

    # 只做环境准备检查：打印 CHECK 行后退出，不写入任何文件。
    # 安装器界面用它画「环境准备」清单，人也可以直接跑来看结论。
    [switch]$CheckOnly
)

$ErrorActionPreference = 'Stop'

# 让 stdout 走 UTF-8。否则 Windows PowerShell 5.1 会按系统 ANSI 代码页输出，
# 被 aicrm-setup.exe 按 UTF-8 读时中文全变成方块。
try { [Console]::OutputEncoding = [Text.Encoding]::UTF8 } catch { }

# 生成 32 字节（64 位十六进制）随机令牌。
# 不用 [Convert]::ToHexString —— 那是 .NET 5+ 才有的 API，只有 Windows PowerShell 5.1
# 的机器上会直接报 "does not contain a method named 'ToHexString'"。
function New-HexToken {
    $bytes = New-Object byte[] 32
    [Security.Cryptography.RandomNumberGenerator]::Create().GetBytes($bytes)
    ($bytes | ForEach-Object { $_.ToString('x2') }) -join ''
}

# 可执行文件来源：优先 target\release（开发机），否则取脚本同级（部署包）。
function Resolve-Binary([string]$name) {
    $fromTarget = Join-Path $PSScriptRoot "target\release\$name"
    if (Test-Path -LiteralPath $fromTarget) { return $fromTarget }
    $beside = Join-Path $PSScriptRoot $name
    if (Test-Path -LiteralPath $beside) { return $beside }
    throw "找不到 $name（既不在 target\release 也不在脚本同级目录）"
}

# 开机自启项的位置：环境准备要检查它可写，所以先给出来。其余路径放在环境准备**之后**
# 再算 —— 它们都是 Join-Path 出来的，盘符不存在之类的低级错误会当场抛错，
# 不该挡在检查前面（检查的作用就是把这些原因说清楚）。
$runKey      = 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Run'

if (-not $ToolSource) {
    $inProject = Join-Path $PSScriptRoot 'src\tools'
    $ToolSource = if (Test-Path -LiteralPath $inProject) { $inProject } else { Join-Path $PSScriptRoot 'tools' }
}
if (-not $ExportProgram) {
    $ExportProgram = Join-Path $ToolSource 'aicrm-export-new.exe'
}
if (-not (Test-Path -LiteralPath $ExportProgram -PathType Leaf)) {
    throw "找不到 Markdown 导出程序：$ExportProgram（用 -ExportProgram 指定路径）"
}

# ---- 环境准备（前置校验）----
# 一台机器能不能跑起来，先查清楚再动磁盘：装完了却用不了，比装不上更难查
# （实测踩过：缺 VC++ 运行库、磁盘写满、旧进程占着文件）。
#
# 输出协议：一行一条 `CHECK|级别|标题|pass/fail|说明` —— 安装器界面直接解析它画清单，
# 人看也一样清楚；失败另外写进 install.err.log。
# 级别：required = 不通过就不装；advisory = 只提醒，不拦。
# 日志位置：正常在安装目录，盘符都不存在这类情况退回临时目录 —— 报错本身不能失败。
try { $errorLog = Join-Path $InstallDir 'install.err.log' }
catch { $errorLog = Join-Path $env:TEMP 'AICRM-install.err.log' }

function Write-InstallLog([string]$message) {
    try {
        if (-not (Test-Path -LiteralPath $InstallDir)) {
            New-Item -ItemType Directory -Force -Path $InstallDir -ErrorAction SilentlyContinue | Out-Null
        }
        $stamp = (Get-Date).ToUniversalTime().ToString('yyyy-MM-dd HH:mm:ss') + 'Z'
        Add-Content -LiteralPath $errorLog -Value "[$stamp] $message" -Encoding UTF8 -ErrorAction SilentlyContinue
    }
    catch { }
}

function Stop-Install([string]$message) {
    Write-InstallLog "安装未完成：$message"
    throw $message
}

# 结果用脚本级变量回传：检查函数只负责打印 CHECK 行，调用方据此决定拦不拦。
$script:PreflightBlocked = $false
$script:PreflightFirstFailure = ''
$script:CrtSource = ''

function Add-Preflight([string]$level, [string]$title, [bool]$passed, [string]$detail) {
    if (-not $passed -and $level -eq 'required') {
        $script:PreflightBlocked = $true
        if (-not $script:PreflightFirstFailure) {
            $script:PreflightFirstFailure = "$title —— $detail"
        }
    }
    $status = if ($passed) { 'pass' } else { 'fail' }
    Write-Output ("CHECK|{0}|{1}|{2}|{3}" -f $level, $title, $status, $detail)
}

function Invoke-Preflight {
    # 1) 系统版本：Win10 1809 / Server 2019（内部版本 17763）起步 —— 再早的系统上
    #    托盘、长路径、TLS 都会出问题，与其装完再猜，不如当场说清楚。
    $version = Get-ItemProperty 'HKLM:\SOFTWARE\Microsoft\Windows NT\CurrentVersion'
    $build = [int]$version.CurrentBuildNumber
    if ($build -le 0) { $build = [Environment]::OSVersion.Version.Build }
    Add-Preflight 'required' '系统版本' ($build -ge 17763) "Windows 内部版本 $build（需要 17763 或更高）"

    # 2) 磁盘空间：载荷解出来约 92MB，加上采集产物留 300MB 余量。
    $drive = (Split-Path -Qualifier $InstallDir).TrimEnd(':', '\')
    $free = -1
    try { $free = (Get-PSDrive -Name $drive -ErrorAction Stop).Free }
    catch { }
    if ($free -lt 0) {
        Add-Preflight 'required' '磁盘空间' $false "读不到 $drive 盘的可用空间（盘符是否存在 / 是否可访问？）"
    }
    else {
        Add-Preflight 'required' '磁盘空间' ($free -ge 300MB) ("{0} 盘可用 {1:N0}MB（需要 300MB 以上）" -f $drive, ($free / 1MB))
    }

    # 3) 安装目录可写。我们是按用户安装、不需要管理员权限 —— 但这两处必须能写。
    try {
        New-Item -ItemType Directory -Force -Path $InstallDir | Out-Null
        $probe = Join-Path $InstallDir '.preflight-write-probe'
        [IO.File]::WriteAllText($probe, 'ok')
        Remove-Item -LiteralPath $probe -Force
        Add-Preflight 'required' '安装目录可写' $true $InstallDir
    }
    catch {
        Add-Preflight 'required' '安装目录可写' $false "$InstallDir（$($_.Exception.Message)）"
    }

    # 4) 开机自启项可写
    try {
        New-Item -Path $runKey -Force | Out-Null
        Set-ItemProperty -Path $runKey -Name 'AICRM-PreflightProbe' -Value 'probe'
        Remove-ItemProperty -Path $runKey -Name 'AICRM-PreflightProbe' -Force
        Add-Preflight 'required' '开机自启可注册' $true 'HKCU Run'
    }
    catch {
        Add-Preflight 'required' '开机自启可注册' $false $_.Exception.Message
    }

    # 5) VC++ 运行库：第三方程序（AICRM-Key.exe / aicrm-export-new.exe）改不了链接
    #    方式，只能在 exe 同目录放一份（应用本地装配）。随包带一份，本机装了也行。
    $crt = Join-Path $ToolSource 'vcruntime140.dll'
    if (-not (Test-Path -LiteralPath $crt -PathType Leaf)) {
        $systemCrt = Join-Path $env:WINDIR 'System32\vcruntime140.dll'
        if (Test-Path -LiteralPath $systemCrt -PathType Leaf) { $crt = $systemCrt }
    }
    $crtFound = Test-Path -LiteralPath $crt -PathType Leaf
    $script:CrtSource = if ($crtFound) { $crt } else { '' }
    Add-Preflight 'required' 'VC++ 运行库' $crtFound $(if ($crtFound) {
        "来源：$crt"
    } else {
        '安装包与这台机器上都没有 vcruntime140.dll，采集用的第三方程序起不来'
    })

    # 6) 云端是否可达：只影响"装完能不能立刻用"，不拦。
    if ($CloudUrl) {
        $reachable = $false
        $detail = "无法连接 $CloudUrl"
        try {
            Invoke-WebRequest -Uri $CloudUrl.TrimEnd('/') -Method Head -TimeoutSec 8 -UseBasicParsing | Out-Null
            $reachable = $true
            $detail = "已连接 $CloudUrl"
        }
        catch {
            if ($_.Exception.Response) {
                $reachable = $true
                $detail = "已连接 $CloudUrl（HTTP $([int]$_.Exception.Response.StatusCode)）"
            }
        }
        Add-Preflight 'advisory' '云端可达' $reachable $detail
    }

    # 7) 微信是否安装：装机不必须，采集才需要，所以只提醒。
    $weixin = @()
    foreach ($root in @($env:ProgramFiles, [Environment]::GetEnvironmentVariable('ProgramFiles(x86)'))) {
        if ($root) {
            $weixin += (Join-Path $root 'Tencent\Weixin\Weixin.exe')
            $weixin += (Join-Path $root 'Tencent\WeChat\WeChat.exe')
        }
    }
    $weixin = @($weixin | Where-Object { Test-Path -LiteralPath $_ -PathType Leaf })
    Add-Preflight 'advisory' '微信已安装' ($weixin.Count -gt 0) $(if ($weixin.Count -gt 0) {
        $weixin[0]
    } else {
        '没找到微信；装好客户端之后再装微信也可以'
    })
}

Invoke-Preflight
if ($script:PreflightBlocked) { Write-InstallLog "环境准备未通过：$script:PreflightFirstFailure" }

# 只做检查：把结论交给调用方（安装器界面 / 人），一个字节都不动。
if ($CheckOnly) {
    if ($script:PreflightBlocked) { exit 1 }
    exit 0
}
if ($script:PreflightBlocked) { Stop-Install "环境准备未通过：$script:PreflightFirstFailure" }

# 开发机从源码构建；部署包里没有 Cargo.toml，直接用随包的可执行文件。
# 放在环境准备之后：检查没过就不该先花几分钟编译（-CheckOnly 更是一行代码都不动）。
# 安装目录里的各项路径也在这里才算出来（见文件开头的说明）。
$exeSource     = Resolve-Binary 'aicrm_windows_api.exe'
$consoleSource = Resolve-Binary 'aicrm-console.exe'
$exe           = Join-Path $InstallDir 'aicrm_windows_api.exe'
$consoleExe    = Join-Path $InstallDir 'aicrm-console.exe'
$tools       = Join-Path $InstallDir 'tools'
$data        = Join-Path $InstallDir 'data'
$tokenFile   = Join-Path $InstallDir 'api.token'

# 密钥目录允许不存在：新机器上密钥还没采集，AICRM-Key.exe 会往里写。
if (-not (Test-Path -LiteralPath $KeyDir -PathType Container)) {
    New-Item -ItemType Directory -Force -Path $KeyDir | Out-Null
    Write-Output "密钥目录不存在，已创建：$KeyDir"
}

$manifest = Join-Path $PSScriptRoot 'Cargo.toml'
if (Test-Path -LiteralPath $manifest) {
    if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
        Stop-Install '开发机安装需要 Rust 工具链（cargo 不在 PATH 里）。请把 %USERPROFILE%\.cargo\bin 加进 PATH，或改用打包好的安装程序。'
    }
    & cargo build --release --locked --manifest-path $manifest
    if ($LASTEXITCODE -ne 0) { throw 'Rust 编译失败' }
}

New-Item -ItemType Directory -Force -Path $InstallDir, $tools, $data | Out-Null
# 覆盖前必须停掉还在跑的旧进程：界面和托盘开着的时候，Copy-Item 会直接撞上
# "文件正由另一进程使用" 而中断安装。安装器自身不在列表里 —— 停自己会让脚本当场断掉。
$stopNames = @('aicrm_windows_api.exe', 'aicrm-console.exe', 'aicrm-login.exe')
foreach ($name in $stopNames) {
    Get-CimInstance Win32_Process -Filter "Name = '$name'" |
        Where-Object { $_.ExecutablePath -like "$InstallDir*" } |
        ForEach-Object { Stop-Process -Id $_.ProcessId -Force -ErrorAction SilentlyContinue }
}
Start-Sleep -Seconds 1    # 给系统一点时间把文件句柄放掉

# 已下线的旧程序：登录窗已并入主控台、托盘更早已并入、登录启动器指向的程序已不存在。
# 顺手清掉，别在安装目录留尸体。
foreach ($obsolete in @('aicrm-login.exe', 'aicrm-tray.exe', '登录.cmd')) {
    $obsoletePath = Join-Path $InstallDir $obsolete
    if (Test-Path -LiteralPath $obsoletePath) {
        Remove-Item -LiteralPath $obsoletePath -Force -ErrorAction SilentlyContinue
    }
}

Copy-Item -LiteralPath $exeSource -Destination $exe -Force
Copy-Item -LiteralPath $consoleSource -Destination $consoleExe -Force
# 把自己也留一份：以后要改地址 / 修安装时，客户机上就有脚本可用。
Copy-Item -LiteralPath $PSCommandPath -Destination (Join-Path $InstallDir 'install.ps1') -Force
# 自更新执行者：客户端收到 agent.update 后靠它替换 exe 并重启。原生 exe，不经解释器。
$updateSource = Resolve-Binary 'aicrm-update.exe'
Copy-Item -LiteralPath $updateSource -Destination (Join-Path $InstallDir 'aicrm-update.exe') -Force
# 服务启动时会校验 tools 目录里这两个程序存在；名字必须与 service 模块里的常量一致。
Copy-Item -LiteralPath (Join-Path $ToolSource 'AICRM-Key.exe') -Destination (Join-Path $tools 'AICRM-Key.exe') -Force
Copy-Item -LiteralPath $ExportProgram -Destination (Join-Path $tools 'aicrm-export-new.exe') -Force

# VC++ 运行库（来源已在环境准备的第 5 项里确认过）：第三方程序是 MSVC 动态链接的，
# 客户机没装 VC++ 时它们起不来；把 DLL 放到 exe 同目录即可（应用本地装配）——
# 不需要管理员、不需要联网、不需要重启。我们自己那几个程序已改成静态链接
# （见 .cargo/config.toml），带上它只是多一层保险。
Copy-Item -LiteralPath $script:CrtSource -Destination (Join-Path $tools 'vcruntime140.dll') -Force
Copy-Item -LiteralPath $script:CrtSource -Destination (Join-Path $InstallDir 'vcruntime140.dll') -Force

# 本地 API 的 Bearer token（安装时生成一次，之后不换）
if (-not (Test-Path -LiteralPath $tokenFile)) {
    $token = New-HexToken
    [IO.File]::WriteAllText($tokenFile, $token, [Text.Encoding]::ASCII)
}

# ---- 云端地址 ----
# 写进文件而不是只钉在命令行/自启项里：服务每轮心跳都重读它 —— 换服务器、
# 换域名只要改这个文件再重启服务即可，不用重装。
# 没有"设备身份"要落：2026-10-10 取消设备绑定后，客户端与云端的关系只有
# 一句「哪个账号在我这里登录着」，登录由客户端界面完成，安装这一环不问账号。
$cloudUrlFile = Join-Path $InstallDir 'cloud.url'
if ($CloudUrl) {
    [IO.File]::WriteAllText($cloudUrlFile, $CloudUrl.TrimEnd('/'), [Text.Encoding]::ASCII)
}

# ---- 收紧敏感文件的权限 ----
$identityName = [Security.Principal.WindowsIdentity]::GetCurrent().Name
foreach ($sensitive in @($tokenFile)) {
    if (-not (Test-Path -LiteralPath $sensitive)) { continue }
    & icacls $sensitive /inheritance:r /grant:r "${identityName}:(F)" '*S-1-5-18:(F)' '*S-1-5-32-544:(F)' | Out-Null
    if ($LASTEXITCODE -ne 0) { throw "无法限制文件权限：$sensitive" }
}
# 导出目录**不再**断继承收紧：它位于 %LOCALAPPDATA% 下，父目录的 ACL 已经把
# 其它用户挡在外面了。而"收紧到安装者"会造成一个真实的坑 —— IT 用管理员账号
# 给客户部署之后，客户自己登录打开数据目录只会看到"你当前无权访问该文件夹"。
# 真正敏感的是上面的令牌文件与登录后写入的登录令牌（api.refresh），都单独收紧。
if (-not (Test-Path -LiteralPath $data)) { New-Item -ItemType Directory -Path $data -Force | Out-Null }

# ---- 桌面与开始菜单快捷方式 ----
# 客户从桌面图标启动界面；托盘里常驻，不需要每次手动开。
$shortcutTargets = @(
    (Join-Path ([Environment]::GetFolderPath('Desktop')) 'AICRM.lnk'),
    (Join-Path ([Environment]::GetFolderPath('Programs')) 'AICRM.lnk')
)
$shell = New-Object -ComObject WScript.Shell
foreach ($shortcutPath in $shortcutTargets) {
    try {
        $link = $shell.CreateShortcut($shortcutPath)
        $link.TargetPath = $consoleExe
        $link.WorkingDirectory = $InstallDir
        $link.IconLocation = "$consoleExe,0"
        $link.Description = 'AICRM'
        $link.Save()
    }
    catch {
        # 快捷方式失败不该挡住安装 —— 自启已经写好，重开机能起来。
        Write-Warning "创建快捷方式失败 $shortcutPath：$_"
    }
}

# ---- 注册自启并立即启动 ----
# 自启项默认**只写一行 exe 路径**：服务从自身所在目录读 tools/accounts/data/凭据文件，
# 云端地址读 cloud.url、登录令牌读 api.token —— 参数拼装整体消失，
# 「改了文件不重装没用」这类坑从结构上不再存在。
# 只有非常规配置才继续写参数：自定义密钥目录 / 监听地址、额外的网页来源。
$extraArguments = ''
if ($KeyDir.TrimEnd('\') -ne (Join-Path $InstallDir 'accounts').TrimEnd('\')) {
    $extraArguments += ' --key-dir "{0}"' -f $KeyDir
}
if ($Bind -ne '127.0.0.1:8765') {
    $extraArguments += ' --bind "{0}"' -f $Bind
}
if ($AllowOrigin) {
    foreach ($origin in ($AllowOrigin -split ',')) {
        if ($origin.Trim()) { $extraArguments += ' --allow-origin "{0}"' -f $origin.Trim() }
    }
}
New-Item -Path $runKey -Force | Out-Null
Set-ItemProperty -Path $runKey -Name 'AICRM-Windows-API' -Value ('"{0}"{1}' -f $exe, $extraArguments)
if ($extraArguments) {
    Start-Process -FilePath $exe -ArgumentList $extraArguments.Trim() -WorkingDirectory $InstallDir -WindowStyle Hidden
} else {
    Start-Process -FilePath $exe -WorkingDirectory $InstallDir -WindowStyle Hidden
}

# 主窗口自带通知区托盘（同一个进程），开机只静默进托盘，不弹窗口打扰客户。
Set-ItemProperty -Path $runKey -Name 'AICRM-客户端' -Value ('"{0}" --hidden' -f $consoleExe)
# 但装完这一次要弹出来，不然客户不知道装了什么
Start-Process -FilePath $consoleExe -WorkingDirectory $InstallDir

# ---- 快捷方式：桌面 + 开始菜单，客户唯一需要认识的入口 ----
# 由 aicrm-update.exe 用 COM IShellLink 建（纯 Rust，不经脚本宿主）——
# 客户机上脚本宿主常被安全软件拦掉，安装与自更新共用同一份实现。
# 它同时会把历史遗留的重复入口与过期安装包收走。
& (Join-Path $InstallDir 'aicrm-update.exe') --ensure-shortcut --install-dir $InstallDir | Out-Null

$ready = $false
for ($i = 0; $i -lt 30; $i++) {
    Start-Sleep -Milliseconds 500
    try {
        $headers = @{ Authorization = 'Bearer ' + [IO.File]::ReadAllText($tokenFile).Trim() }
        $response = Invoke-RestMethod -Uri "http://$Bind/api/aicrm/export/version" -Headers $headers -TimeoutSec 2
        if ($response.success) { $ready = $true; break }
    }
    catch { }
}
if (-not $ready) { throw "启动项已写入，但接口未在 $Bind 就绪；请检查端口和进程状态" }

Write-Output ''
Write-Output "AICRM Windows API 已安装并启动：$Bind"
Write-Output "开机登录自启：HKCU Run\AICRM-Windows-API"
Write-Output "密钥目录：$KeyDir"
Write-Output "本地 API token：$tokenFile"
Write-Output "主窗口：$consoleExe"
Write-Output "快捷方式：AICRM.lnk（桌面 + 开始菜单）"
if ($CloudUrl) {
    Write-Output "云端地址：$cloudUrlFile（服务每轮重读；换服务器改这里再重启服务即可）"
    Write-Output "  下一步：打开客户端，用云端账号登录一次即可接入命令通道"
}
else { Write-Output '提示：加 -CloudUrl <云端基地址> 后，客户端登录一次即可接入云端命令通道' }
