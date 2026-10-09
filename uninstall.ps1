param([string]$InstallDir = (Join-Path $env:LOCALAPPDATA 'AICRM-Windows-API'))

$runKey = 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Run'
Remove-ItemProperty -Path $runKey -Name 'AICRM-Windows-API' -ErrorAction SilentlyContinue
Remove-ItemProperty -Path $runKey -Name 'AICRM-客户端' -ErrorAction SilentlyContinue
$exe = Join-Path $InstallDir 'aicrm_windows_api.exe'
$console = Join-Path $InstallDir 'aicrm-console.exe'
foreach ($name in 'aicrm_windows_api.exe', 'aicrm-console.exe') {
    $target = Join-Path $InstallDir $name
    $existing = Get-CimInstance Win32_Process -Filter "Name = '$name'" |
        Where-Object { $_.ExecutablePath -eq $target }
    foreach ($process in $existing) { Stop-Process -Id $process.ProcessId -Force }
}
Write-Output '已移除开机自启并停止客户端（服务与主窗口）；程序、密钥和数据文件保留。'
