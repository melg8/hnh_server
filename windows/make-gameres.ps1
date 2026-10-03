# Generate the gameres resource pack for the server's HTTP resource port.
# Overlay order: lib/haven-res.jar first, then res/compiled on top.

$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $PSScriptRoot   # repo root
$jar = Join-Path $root "lib\haven-res.jar"
$out = Join-Path $root "gameres"
$tmp = Join-Path $env:TEMP ("hnh-gameres-" + [guid]::NewGuid().ToString("N"))

if (-not (Test-Path $jar)) {
    Write-Error "haven-res.jar not found at $jar"
    exit 1
}

New-Item -ItemType Directory -Force -Path $tmp | Out-Null
Write-Host "Extracting $jar ..."
# .jar is a zip; rename-copy avoids ExtensionType filtering.
$zipCopy = Join-Path $tmp "res.zip"
Copy-Item $jar $zipCopy
Expand-Archive -Path $zipCopy -DestinationPath $tmp -Force

New-Item -ItemType Directory -Force -Path $out | Out-Null
Write-Host "Copying res/* into gameres/ ..."
if (Test-Path (Join-Path $tmp "res")) {
    Copy-Item -Path (Join-Path $tmp "res\*") -Destination $out -Recurse -Force -ErrorAction SilentlyContinue
}

$compiled = Join-Path $root "res\compiled"
if (Test-Path $compiled) {
    Write-Host "Overlaying res/compiled ..."
    Copy-Item -Path (Join-Path $compiled "*") -Destination $out -Recurse -Force -ErrorAction SilentlyContinue
}

Remove-Item -Recurse -Force $tmp
$count = (Get-ChildItem -Recurse -File $out | Measure-Object).Count
Write-Host "gameres ready: $count files in $out"
