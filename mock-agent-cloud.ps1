param(
    [int]$Port = 9414,
    [string]$ExportDir = '',
    [string]$LogFile = '',
    [int]$MaxCommands = 1
)

# 假云端命令通道：应答 aicrm_windows_api 的轮询，下发一次 wechat.export，再接收结果。
# 用于在没有真云端时验证「云端下发命令 → 客户端执行 → 回传结果」这条链路。

$listener = [System.Net.Sockets.TcpListener]::new([System.Net.IPAddress]::Loopback, $Port)
$listener.Start()
Write-Host "假云端命令通道已就绪：http://127.0.0.1:$Port"
$served = 0

function Send-Response($stream, $status, $json) {
    $body = [Text.Encoding]::UTF8.GetBytes($json)
    $head = [Text.Encoding]::ASCII.GetBytes("HTTP/1.1 $status`r`nContent-Type: application/json`r`nContent-Length: $($body.Length)`r`nConnection: close`r`n`r`n")
    $stream.Write($head, 0, $head.Length)
    $stream.Write($body, 0, $body.Length)
    $stream.Flush()
}

try {
    while ($served -lt $MaxCommands) {
        $client = $listener.AcceptTcpClient()
        try {
            Start-Sleep -Milliseconds 200
            $stream = $client.GetStream()
            $buffer = New-Object byte[] 65536
            $read = $stream.Read($buffer, 0, $buffer.Length)
            $request = [Text.Encoding]::UTF8.GetString($buffer, 0, $read)
            if ($LogFile) { [IO.File]::AppendAllText($LogFile, "$request`r`n====`r`n") }

            $firstLine = ($request -split "`r`n")[0]
            $parts = $firstLine -split ' '
            $method = $parts[0]; $path = $parts[1]
            Write-Host "$method $path"

            if ($method -eq 'POST' -and $path -like '*commands/poll*') {
                $dir = if ($ExportDir) { $ExportDir } else { Join-Path $env:TEMP 'aicrm-agent-export' }
                $dirJson = $dir.Replace('\', '\\')
                $json = '{"code":0,"message":"success","data":{"command":{"id":"cmd-001","type":"wechat.export","payload":{"day":2,"export_dir":"' + $dirJson + '"}}}}'
                Send-Response $stream '200 OK' $json
                Write-Host "  -> 已下发 wechat.export，目标目录 $dir"
            }
            elseif ($method -eq 'PUT' -and $path -like '*result*') {
                $json = '{"code":0,"message":"success","data":null}'
                Send-Response $stream '200 OK' $json
                Write-Host "  -> 已接收结果"
                $served++
            }
            else {
                Send-Response $stream '404 Not Found' '{"code":404,"message":"not found"}'
            }
        } catch {
            Write-Host "  处理出错: $($_.Exception.Message)"
        } finally {
            $client.Close()
        }
    }
} finally {
    $listener.Stop()
    Write-Host "假云端退出（已服务 $served 条命令）"
}
