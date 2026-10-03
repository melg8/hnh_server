# wait-server.ps1 - block until the local hnh-server TCP services accept
# connections (auth 1871 + resources 1872). Used by run-client.bat so the
# client never races a still-booting server: that race surfaces later as a
# delayed "Load error in resource gfx/hud/fbtn" crash right after login.
param(
    [int]$TimeoutSec = 300
)

$ports = @(1871, 1872)
$deadline = (Get-Date).AddSeconds($TimeoutSec)

while ($true) {
    $allUp = $true
    foreach ($p in $ports) {
        $c = New-Object Net.Sockets.TcpClient
        try {
            $ar = $c.BeginConnect('127.0.0.1', $p, $null, $null)
            if (-not $ar.AsyncWaitHandle.WaitOne(500) -or -not $c.Connected) {
                $allUp = $false
            }
        }
        catch {
            $allUp = $false
        }
        finally {
            $c.Close()
        }
        if (-not $allUp) { break }
    }
    if ($allUp) { exit 0 }
    if ((Get-Date) -gt $deadline) { exit 1 }
    Start-Sleep -Milliseconds 500
}
