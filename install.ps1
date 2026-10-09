param(
    [string]$InstallDir = (Join-Path $env:LOCALAPPDATA 'AICRM-Windows-API'),
    # 密钥目录。默认放在安装目录下 —— 部署到别的机器时密钥应由本机自行采集，
    # 不能沿用开发机的项目路径。开发机想继续用既有密钥就显式传 -KeyDir。
    [string]$KeyDir = (Join-Path $InstallDir 'accounts'),
    [string]$Bind = '127.0.0.1:8765',

    # ---- 登录窗口 ----
    # 云端登录接口完整地址；给了就生成「登录.cmd」启动器。
    [string]$Endpoint = '',

    # ---- 命令通道（云端控制本机取密钥/导出）----
    # 云端基地址，例如 https://xxx.trycloudflare.com；给了才启用轮询。
    [string]$CloudUrl = '',
    # 云端网页端签发预置码时一并返回的设备的 ID。
    # 必须原样使用：云端要求 deviceId 与预置码成对匹配，自己随机生成会被 410 拒绝。
    [string]$DeviceId = '',
    # 云端网页端签发的短时预置码。留空表示只装成本地 API，不接云端。
    [string]$ProvisionCode = '',
    [string]$DeviceName = $env:COMPUTERNAME,
    # 允许调用本机客户端接口的网页来源（客户的云端域名）。可重复传，或逗号分隔。
    [string]$AllowOrigin = '',

    # ---- 辅助程序来源 ----
    # 留空则自动判定：开发机用 src\tools，部署包用脚本同级的 tools\。
    [string]$ToolSource = '',
    # Markdown 导出程序。默认在 ToolSource 里找；找不到就报错，不静默降级。
    [string]$ExportProgram = '',
    # 归档导出程序（出 messages.json）。数据采集（wechat.collect / POST /api/aicrm/collect）
    # 依赖它；默认在 ToolSource 里找 AICRM-Export.exe。
    [string]$ArchiveProgram = ''
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

$exeSource     = Resolve-Binary 'aicrm_windows_api.exe'
$loginSource   = Resolve-Binary 'aicrm-login.exe'
$consoleSource = Resolve-Binary 'aicrm-console.exe'
$exe           = Join-Path $InstallDir 'aicrm_windows_api.exe'
$loginExe      = Join-Path $InstallDir 'aicrm-login.exe'
$consoleExe    = Join-Path $InstallDir 'aicrm-console.exe'
$tools       = Join-Path $InstallDir 'tools'
$data        = Join-Path $InstallDir 'data'
$tokenFile   = Join-Path $InstallDir 'api.token'
$deviceFile  = Join-Path $InstallDir 'device.id'
$deviceTokenFile = Join-Path $InstallDir 'device.token'
$runKey      = 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Run'

if (-not $ToolSource) {
    $inProject = Join-Path $PSScriptRoot 'src\tools'
    $ToolSource = if (Test-Path -LiteralPath $inProject) { $inProject } else { Join-Path $PSScriptRoot 'tools' }
}
# 密钥目录允许不存在：新机器上密钥还没采集，AICRM-Key.exe 会往里写。
if (-not (Test-Path -LiteralPath $KeyDir -PathType Container)) {
    New-Item -ItemType Directory -Force -Path $KeyDir | Out-Null
    Write-Output "密钥目录不存在，已创建：$KeyDir"
}
if (-not $ExportProgram) {
    $ExportProgram = Join-Path $ToolSource 'aicrm-export-new.exe'
}
if (-not (Test-Path -LiteralPath $ExportProgram -PathType Leaf)) {
    throw "找不到 Markdown 导出程序：$ExportProgram（用 -ExportProgram 指定路径）"
}
# 归档导出程序。数据采集要用它出 messages.json；少了它本地接口仍然活着，
# 但一点「数据采集」就报「辅助程序启动失败」—— 这种"装得上、用不了"的故障
# 最难排查，所以宁可安装时就拦住。
if (-not $ArchiveProgram) {
    $ArchiveProgram = Join-Path $ToolSource 'AICRM-Export.exe'
}
if (-not (Test-Path -LiteralPath $ArchiveProgram -PathType Leaf)) {
    throw "找不到归档导出程序：$ArchiveProgram（用 -ArchiveProgram 指定路径）。数据采集依赖它，不能省略。"
}

# 开发机从源码构建；部署包里没有 Cargo.toml，直接用随包的可执行文件。
$manifest = Join-Path $PSScriptRoot 'Cargo.toml'
if (Test-Path -LiteralPath $manifest) {
    & cargo build --release --locked --manifest-path $manifest
    if ($LASTEXITCODE -ne 0) { throw 'Rust 编译失败' }
}

New-Item -ItemType Directory -Force -Path $InstallDir, $tools, $data | Out-Null
$existing = Get-CimInstance Win32_Process -Filter "Name = 'aicrm_windows_api.exe'" |
    Where-Object { $_.ExecutablePath -eq $exe }
foreach ($process in $existing) { Stop-Process -Id $process.ProcessId -Force }

Copy-Item -LiteralPath $exeSource -Destination $exe -Force
Copy-Item -LiteralPath $loginSource -Destination $loginExe -Force
Copy-Item -LiteralPath $consoleSource -Destination $consoleExe -Force
# 服务启动时会校验 tools 目录里这两个程序存在；名字必须与 service 模块里的常量一致。
Copy-Item -LiteralPath (Join-Path $ToolSource 'AICRM-Key.exe') -Destination (Join-Path $tools 'AICRM-Key.exe') -Force
Copy-Item -LiteralPath $ExportProgram -Destination (Join-Path $tools 'aicrm-export-new.exe') -Force
# AICRM-Export.exe 只在采集时用，不在启动校验里 —— 但少了它采集必失败，所以一并复制。
Copy-Item -LiteralPath $ArchiveProgram -Destination (Join-Path $tools 'AICRM-Export.exe') -Force

# 本地 API 的 Bearer token（安装时生成一次，之后不换）
if (-not (Test-Path -LiteralPath $tokenFile)) {
    $token = New-HexToken
    [IO.File]::WriteAllText($tokenFile, $token, [Text.Encoding]::ASCII)
}

# ---- 设备身份与云端配对 ----
# 注意：PowerShell 变量名大小写不敏感，这里不能用 $deviceId 承接参数 $DeviceId ——
# 初始化赋值会把参数本身清空。所以本地用一个明确不同的名字。
$cloudDeviceId = ''
$cloudArguments = ''
if ($CloudUrl) {
    if (Test-Path -LiteralPath $deviceFile) {
        # 重装沿用既有身份，避免云端把同一台机器认成两台
        $cloudDeviceId = ([IO.File]::ReadAllText($deviceFile)).Trim()
    }
    elseif ($DeviceId) {
        $cloudDeviceId = $DeviceId.Trim()
        [IO.File]::WriteAllText($deviceFile, $cloudDeviceId, [Text.Encoding]::ASCII)
    }
    else {
        throw '首次接入云端需要 -DeviceId（与 -ProvisionCode 配套，由云端网页端签发时一并给出）'
    }

    if (Test-Path -LiteralPath $deviceTokenFile) {
        Write-Output '设备此前已配对，沿用已有设备令牌。'
    }
    elseif (-not $ProvisionCode) {
        throw '首次接入云端需要 -ProvisionCode（从云端网页端「绑定微信账号」处获取）'
    }
    else {
        # 设备令牌在本机生成，云端只保存 SHA-256，不保存明文。
        $deviceToken = New-HexToken
        $payload = @{
            deviceId    = $cloudDeviceId
            code        = $ProvisionCode
            deviceToken = $deviceToken
            name        = $DeviceName
        } | ConvertTo-Json -Compress
        try {
            $response = Invoke-RestMethod -Method Post -Uri "$($CloudUrl.TrimEnd('/'))/api/agent/v1/provision" `
                -ContentType 'application/json' -Body $payload -TimeoutSec 30
        }
        catch {
            throw "云端配对失败：$($_.Exception.Message)。预置码有效期只有 10 分钟，过期请重新签发。"
        }
        if (-not $response.data.provisioned) { throw "云端未确认配对：$($response | ConvertTo-Json -Compress)" }
        [IO.File]::WriteAllText($deviceTokenFile, $deviceToken, [Text.Encoding]::ASCII)
        Write-Output "设备已配对：$cloudDeviceId（$DeviceName）"
    }

    # 云端地址写进文件，而不是只钉在命令行/自启项里。
    # 隧道地址变了（cloudflared 重开、域名轮换）只要改这个文件再重启服务即可，不用重装。
    $cloudUrlFile = Join-Path $InstallDir 'cloud.url'
    [IO.File]::WriteAllText($cloudUrlFile, $CloudUrl.TrimEnd('/'), [Text.Encoding]::ASCII)
    $cloudArguments = ' --cloud-url-file "{0}" --device-token-file "{1}" --device-id "{2}"' -f $cloudUrlFile, $deviceTokenFile, $cloudDeviceId
}

# ---- 收紧敏感文件的权限 ----
$identityName = [Security.Principal.WindowsIdentity]::GetCurrent().Name
foreach ($sensitive in @($tokenFile, $deviceTokenFile, $deviceFile)) {
    if (-not (Test-Path -LiteralPath $sensitive)) { continue }
    & icacls $sensitive /inheritance:r /grant:r "${identityName}:(F)" '*S-1-5-18:(F)' '*S-1-5-32-544:(F)' | Out-Null
    if ($LASTEXITCODE -ne 0) { throw "无法限制文件权限：$sensitive" }
}
# 导出目录**不再**断继承收紧：它位于 %LOCALAPPDATA% 下，父目录的 ACL 已经把
# 其它用户挡在外面了。而"收紧到安装者"会造成一个真实的坑 —— IT 用管理员账号
# 给客户部署之后，客户自己登录打开数据目录只会看到"你当前无权访问该文件夹"。
# 真正敏感的是上面那三个凭据文件（尤其是能冒充设备的 device.token），已单独收紧。
if (-not (Test-Path -LiteralPath $data)) { New-Item -ItemType Directory -Path $data -Force | Out-Null }

# ---- 登录窗口启动器 ----
if ($Endpoint) {
    $launcher = Join-Path $InstallDir '登录.cmd'
    $line = '@"{0}" --endpoint "{1}" --token-file "{2}" --device-file "{3}"' -f $loginExe, $Endpoint, $tokenFile, $deviceFile
    [IO.File]::WriteAllText($launcher, "@echo off`r`n$line`r`n", [Text.Encoding]::Default)
}

# ---- 注册自启并立即启动 ----
$originArgs = ''
if ($AllowOrigin) {
    foreach ($origin in ($AllowOrigin -split ',')) {
        if ($origin.Trim()) { $originArgs += ' --allow-origin "{0}"' -f $origin.Trim() }
    }
}
$arguments = '--tools-dir "{0}" --key-dir "{1}" --data-dir "{2}" --token-file "{3}" --bind "{4}"{5}{6}' -f $tools, $KeyDir, $data, $tokenFile, $Bind, $cloudArguments, $originArgs
New-Item -Path $runKey -Force | Out-Null
Set-ItemProperty -Path $runKey -Name 'AICRM-Windows-API' -Value ('"{0}" {1}' -f $exe, $arguments)
Start-Process -FilePath $exe -ArgumentList $arguments -WorkingDirectory $InstallDir -WindowStyle Hidden

# 主窗口自带通知区托盘（同一个进程），开机只静默进托盘，不弹窗口打扰客户。
Set-ItemProperty -Path $runKey -Name 'AICRM-客户端' -Value ('"{0}" --hidden' -f $consoleExe)
# 但装完这一次要弹出来，不然客户不知道装了什么
Start-Process -FilePath $consoleExe -WorkingDirectory $InstallDir

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
Write-Output "登录窗口：$loginExe"
if ($Endpoint) { Write-Output "登录启动器：$(Join-Path $InstallDir '登录.cmd')" }
else { Write-Output '提示：加 -Endpoint <云端登录地址> 可生成登录启动器' }
if ($CloudUrl) {
    Write-Output "命令通道：已接入 $($CloudUrl.TrimEnd('/'))"
    Write-Output "  设备 ID：$cloudDeviceId"
    Write-Output "  设备令牌：$deviceTokenFile（已限制权限）"
    Write-Output "  云端地址：$cloudUrlFile（隧道地址变了改这里，再重启服务即可）"
}
else { Write-Output '提示：加 -CloudUrl <云端基地址> -ProvisionCode <预置码> 可接入云端命令通道' }
