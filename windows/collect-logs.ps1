# collect-logs.ps1 - bundle everything needed for a bug report into one
# zip: the server log, the client log, and environment info (git rev,
# working-tree state, java/cargo versions). The user sends one file.
#
# Windows PowerShell 5.1 turns native stderr lines redirected with 2>&1
# into error records, which aborts the script under -ErrorAction Stop;
# java -version and cargo --version write to stderr by design. Every
# native call therefore runs through cmd /c, which merges the streams
# before PowerShell sees them, and nothing here aborts the report.
$ErrorActionPreference = 'Continue'

$root = Split-Path -Parent $PSScriptRoot
$logs = Join-Path $root 'logs'
New-Item -ItemType Directory -Force -Path $logs | Out-Null

# Run a command through cmd so its stderr is merged into stdout at the
# cmd level; PowerShell only ever sees stdout lines.
function Get-MergedOutput([string]$cmd) {
    (cmd /c $cmd | Out-String).Trim()
}

$stamp = Get-Date -Format 'yyyyMMdd-HHmmss'
$info = Join-Path $logs "info-$stamp.txt"

"bugreport generated: $(Get-Date -Format o)" | Out-File -FilePath $info -Encoding utf8
"git rev: $(Get-MergedOutput "git -C `"$root`" rev-parse HEAD 2>&1")" | Out-File $info -Append -Encoding utf8
"git changes:" | Out-File $info -Append -Encoding utf8
Get-MergedOutput "git -C `"$root`" status --short 2>&1" | Out-File $info -Append -Encoding utf8
"" | Out-File $info -Append -Encoding utf8
"java:" | Out-File $info -Append -Encoding utf8
Get-MergedOutput "java -version 2>&1" | Out-File $info -Append -Encoding utf8
"cargo:" | Out-File $info -Append -Encoding utf8
Get-MergedOutput "cargo --version 2>&1" | Out-File $info -Append -Encoding utf8

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
