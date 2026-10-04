# wait-server.ps1 - block until the local hnh-server services actually
# answer: auth (tcp/1871) accepts TCP and the resource server (tcp/1872)
# serves a real resource over HTTP. Used by run-client.bat so the client
# never races a still-booting server: that race surfaces later as a
# delayed "Load error in resource gfx/hud/fbtn" crash right after login.
param(
    [int]$TimeoutSec = 300
)

$deadline = (Get-Date).AddSeconds($TimeoutSec)

function Test-AuthTcp {
    # Plain TCP probe; the TLS handshake itself is not required here.
    $c = New-Object Net.Sockets.TcpClient
    try {
        $ar = $c.BeginConnect('127.0.0.1', 1871, $null, $null)
        return ($ar.AsyncWaitHandle.WaitOne(500) -and $c.Connected)
    } catch {
        return $false
    } finally {
        $c.Close()
    }
}

function Test-ResHttp {
    # The resource server must serve an actual resource, not just accept
    # TCP: a listener that is up but not serving is exactly the failure
    # this script exists to catch. gfx/hud/fbtn is part of the custom
    # overlay, so it also proves the gameres overlay was applied.
    try {
        $r = Invoke-WebRequest -UseBasicParsing `
                -Uri 'http://127.0.0.1:1872/gfx/hud/fbtn' -TimeoutSec 2
        return ($r.StatusCode -eq 200)
    } catch {
        return $false
    }
}

while ($true) {
    if ((Test-AuthTcp) -and (Test-ResHttp)) { exit 0 }
    if ((Get-Date) -gt $deadline) { exit 1 }
    Start-Sleep -Milliseconds 500
}
