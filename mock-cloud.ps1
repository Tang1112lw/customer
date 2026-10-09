param(
    [int]$Port = 9413,
    [string]$User = 'demo',
    [string]$Password = '123456',
    # 给了就把每次收到的原始请求追加写入该文件，便于核对契约
    [string]$LogFile = ''
)

# 循环假云端：在没有真云端时演示 / 联调 aicrm-login.exe。响应体必须是客户端要求的
# `{code,message,data}` 信封（令牌在 data.accessToken），否则登录窗口会报"不是信封 / 缺少 data"。
# 账号密码匹配则返回 200，否则返回 401。进程常驻，杀掉即结束。

$listener = [System.Net.Sockets.TcpListener]::new([System.Net.IPAddress]::Loopback, $Port)
$listener.Start()
Write-Host "假云端已就绪：http://127.0.0.1:$Port/api/auth/login"
Write-Host "可用账号：$User / $Password   （其它组合返回 401）"

try {
    while ($true) {
        $client = $listener.AcceptTcpClient()
        try {
            Start-Sleep -Milliseconds 150
            $stream = $client.GetStream()
            $buffer = New-Object byte[] 16384
            $read = $stream.Read($buffer, 0, $buffer.Length)
            $request = [Text.Encoding]::UTF8.GetString($buffer, 0, $read)
            if ($LogFile) { [IO.File]::AppendAllText($LogFile, "$request`r`n----`r`n") }

            $payload = $null
            $split = $request.IndexOf("`r`n`r`n")
            if ($split -ge 0) {
                try { $payload = $request.Substring($split + 4) | ConvertFrom-Json } catch { }
            }

            if ($payload -and $payload.username -eq $User -and $payload.password -eq $Password) {
                $status = '200 OK'
                $json = '{"code":0,"message":"success","data":{"accessToken":"tok_demo_7c41","refreshToken":"tok_demo_refresh","user":{"user":{"name":"演示账号","username":"demo"}}}}'
            } else {
                $status = '401 Unauthorized'
                $json = '{"code":401,"message":"账号或密码错误","data":null}'
            }

            $body = [Text.Encoding]::UTF8.GetBytes($json)
            $head = [Text.Encoding]::ASCII.GetBytes("HTTP/1.1 $status`r`nContent-Type: application/json`r`nContent-Length: $($body.Length)`r`nConnection: close`r`n`r`n")
            $stream.Write($head, 0, $head.Length)
            $stream.Write($body, 0, $body.Length)
            $stream.Flush()
            Start-Sleep -Milliseconds 150
        } catch {
        } finally {
            $client.Close()
        }
    }
} finally {
    $listener.Stop()
}
