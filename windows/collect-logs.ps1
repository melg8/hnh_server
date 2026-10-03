# collect-logs.ps1 - bundle everything needed for a bug report into one
# zip: every logs/*.log plus environment info (git rev, dirty state,
# java/cargo/ant versions, server port status). The user sends one file.
#
# Hard rules learned the hard way (Windows PowerShell 5.1):
# 1. Native stderr redirected with 2>&1 becomes ErrorRecords and aborts
#    scripts under ErrorActionPreference=Stop -> every native call goes
#    through cmd /c, which merges the streams before PowerShell sees them.
# 2. Quoted paths passed as cmd /c arguments get mangled ("no directory
#    given for '-C' option") -> no git -C and no quoted cmd arguments at
#    all; git runs after Push-Location instead.
# 3. The server/client keep their log files open for writing while
#    running -> Compress-Archive (which opens FileShare.Read) fails on
#    them with "file is used by another process". Every log is first
#    copied into a staging folder with FileShare ReadWrite|Delete, and
#    the zip is built from the copies.
# 4. Never claim success without checking the zip exists and is not empty.

param([string]$RepoRoot = '')

$ErrorActionPreference = 'Stop'

# set inside try; kept for the catch block so it can point at the info file
$info = ''

function Add-Info([string]$Text) {
    $Text | Out-File -FilePath $info -Append -Encoding utf8
}

# Native command with stderr merged by cmd itself (never by PowerShell,
# and never with embedded quotes in the command line).
function Merge-Cmd([string]$CommandLine) {
    cmd /c "$CommandLine 2>&1"
}

# Copy a file that another process may hold open for writing.
function Copy-OpenFile([string]$Src, [string]$Dst) {
    $share = [System.IO.FileShare]::ReadWrite -bor [System.IO.FileShare]::Delete
    $in = [System.IO.FileStream]::new($Src,
        [System.IO.FileMode]::Open, [System.IO.FileAccess]::Read, $share)
    try {
        $out = [System.IO.File]::Create($Dst)
        try { $in.CopyTo($out) } finally { $out.Dispose() }
    } finally {
        $in.Dispose()
    }
}

try {
    # --- resolve repo root (parent of the windows\ dir this script is in) ---
    $root = $RepoRoot
    if ([string]::IsNullOrWhiteSpace($root) -and $PSScriptRoot) {
        $root = Split-Path -Parent $PSScriptRoot
    }
    if ([string]::IsNullOrWhiteSpace($root)) {
        $root = Split-Path -Parent (Get-Location).Path
    }
    $root = (Resolve-Path $root).Path
    $logs = Join-Path $root 'logs'
    New-Item -ItemType Directory -Force -Path $logs | Out-Null

    $stamp = Get-Date -Format 'yyyyMMdd-HHmmss'
    $info  = Join-Path $logs "info-$stamp.txt"
    $stage = Join-Path $logs "_stage-$stamp"
    $zip   = Join-Path $logs "bugreport-$stamp.zip"

    "bugreport generated: $(Get-Date -Format o)" | Out-File -FilePath $info -Encoding utf8
    Add-Info "repo: $root"
    Add-Info "user: $env:USERNAME  host: $env:COMPUTERNAME"
    Add-Info "os:   $([System.Environment]::OSVersion.VersionString)"
    Add-Info "ps:   $($PSVersionTable.PSVersion)"

    # git info: cwd-based, no -C, no quoted paths through cmd
    Push-Location $root
    try {
        Add-Info "git rev: $(Merge-Cmd 'git rev-parse --short HEAD')"
        $st = (Merge-Cmd 'git status --short') -join ' | '
        if ($st.Length -gt 400) { $st = $st.Substring(0, 400) + ' ...' }
        Add-Info "git changes: $st"
    } finally {
        Pop-Location
    }

    foreach ($probe in @('java -version', 'cargo --version', 'ant -version')) {
        $name = $probe.Split(' ')[0]
        $out = (Merge-Cmd $probe) -join ' '
        if ([string]::IsNullOrWhiteSpace($out)) { $out = '(no output - not installed?)' }
        Add-Info "${name}: $out"
    }

    # is the server actually up? (listeners on 1870/1871/1872)
    Add-Info '--- listeners ---'
    try {
        $tcp = Get-NetTCPConnection -LocalPort 1871, 1872 -ErrorAction SilentlyContinue
        foreach ($c in $tcp) {
            Add-Info ("tcp {0}:{1} {2} pid={3}" -f $c.LocalAddress, $c.LocalPort, $c.State, $c.OwningProcess)
        }
        $udp = Get-NetUDPEndpoint -LocalPort 1870 -ErrorAction SilentlyContinue
        foreach ($u in $udp) {
            Add-Info ("udp {0}:{1} pid={2}" -f $u.LocalAddress, $u.LocalPort, $u.OwningProcess)
        }
        if (-not $tcp -and -not $udp) {
            Add-Info 'nothing listens on 1870/1871/1872 - the server is DOWN'
        }
    } catch {
        Add-Info 'port probe not available on this system'
    }

    Add-Info '--- collected files ---'

    # drop staging leftovers from earlier failed runs
    Get-ChildItem -Path $logs -Filter '_stage-*' -Directory -ErrorAction SilentlyContinue |
        Remove-Item -Recurse -Force -ErrorAction SilentlyContinue

    New-Item -ItemType Directory -Force -Path $stage | Out-Null
    $found = @(Get-ChildItem -Path $logs -Filter '*.log' -File)
    if ($found.Count -eq 0) {
        Add-Info 'no *.log files found in the logs directory'
    }
    foreach ($f in $found) {
        $dst = Join-Path $stage $f.Name
        try {
            Copy-OpenFile $f.FullName $dst
            Add-Info ("{0}  {1} bytes" -f $f.Name, (Get-Item $dst).Length)
        } catch {
            Add-Info ("{0}  COPY FAILED: {1}" -f $f.Name, $_.Exception.Message)
        }
    }

    # info file is complete now - archive it together with the log copies
    Copy-Item $info -Destination $stage
    Compress-Archive -Path (Join-Path $stage '*') -DestinationPath $zip -Force
    Remove-Item -Path $stage -Recurse -Force

    if (-not (Test-Path $zip)) { throw 'archive was not created' }
    $size = (Get-Item $zip).Length
    if ($size -le 0) { throw 'archive is empty' }

    Write-Host ''
    Write-Host 'Bug report ready - send this single file:' -ForegroundColor Green
    Write-Host "  $zip ($size bytes)"
    exit 0
} catch {
    Write-Host ''
    Write-Host "Bug report FAILED: $($_.Exception.Message)" -ForegroundColor Red
    if ($info -and (Test-Path $info)) {
        Write-Host "Raw info file (still useful): $info"
    }
    exit 1
}
