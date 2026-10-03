# collect-logs.ps1 - bundle everything needed for a bug report into one
# zip: the server log, the client log, and environment info (git rev,
# working-tree state, java/cargo versions). The user sends one file.
$ErrorActionPreference = 'Stop'

$root = Split-Path -Parent $PSScriptRoot
$logs = Join-Path $root 'logs'
New-Item -ItemType Directory -Force -Path $logs | Out-Null

$stamp = Get-Date -Format 'yyyyMMdd-HHmmss'
$info = Join-Path $logs "info-$stamp.txt"

"bugreport generated: $(Get-Date -Format o)" | Out-File -FilePath $info -Encoding utf8
"git rev: $(git -C $root rev-parse HEAD 2>$null)" | Out-File $info -Append -Encoding utf8
"git changes:" | Out-File $info -Append -Encoding utf8
git -C $root status --short 2>$null | Out-File $info -Append -Encoding utf8
"" | Out-File $info -Append -Encoding utf8
"java:" | Out-File $info -Append -Encoding utf8
java -version 2>&1 | Out-File $info -Append -Encoding utf8
"cargo:" | Out-File $info -Append -Encoding utf8
cargo --version 2>&1 | Out-File $info -Append -Encoding utf8

$files = @($info)
foreach ($name in @('server.log', 'client.log')) {
    $p = Join-Path $logs $name
    if (Test-Path $p) { $files += $p }
}

$out = Join-Path $logs "bugreport-$stamp.zip"
Compress-Archive -Path $files -DestinationPath $out -Force
Remove-Item $info -ErrorAction SilentlyContinue

Write-Host ''
Write-Host 'Bug report ready - send this single file:'
Write-Host "  $out"
