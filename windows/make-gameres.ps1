# Generate the gameres resource pack for the server's HTTP resource port.
# Overlay order: lib/haven-res.jar first, then res/compiled on top.
# The output is wiped first (same semantics as server/scripts/
# make-gameres.sh) so files REMOVED from the jar or the overlay never
# linger from a previous revision.

$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $PSScriptRoot   # repo root
$jar = Join-Path $root "lib\haven-res.jar"
$out = Join-Path $root "gameres"
$tmp = Join-Path $env:TEMP ("hnh-gameres-" + [guid]::NewGuid().ToString("N"))

if (-not (Test-Path $jar)) {
    Write-Error "haven-res.jar not found at $jar"
    exit 1
}

# Extract fully BEFORE wiping: a failed extraction must leave the
# existing pack intact (the server keeps serving the old pack).
New-Item -ItemType Directory -Force -Path $tmp | Out-Null
Write-Host "Extracting $jar ..."
# .jar is a zip; rename-copy avoids ExtensionType filtering.
$zipCopy = Join-Path $tmp "res.zip"
Copy-Item $jar $zipCopy
Expand-Archive -Path $zipCopy -DestinationPath $tmp -Force
if (-not (Test-Path (Join-Path $tmp "res"))) {
    Write-Error "jar extraction produced no res/ directory"
    exit 1
}

Write-Host "Rebuilding gameres/ from scratch ..."
if (Test-Path $out) {
    # Preserve the .genrev stamp decision file? No - the caller
    # (start-server.bat) rewrites it after a successful generation.
    Remove-Item -Recurse -Force $out
}
New-Item -ItemType Directory -Force -Path $out | Out-Null
Copy-Item -Path (Join-Path $tmp "res\*") -Destination $out -Recurse -Force

$compiled = Join-Path $root "res\compiled"
if (Test-Path $compiled) {
    Write-Host "Overlaying res/compiled ..."
    Copy-Item -Path (Join-Path $compiled "*") -Destination $out -Recurse -Force
}

Remove-Item -Recurse -Force $tmp
$count = (Get-ChildItem -Recurse -File $out | Measure-Object).Count
Write-Host "gameres ready: $count files in $out"

# The legacy jar pack ships a few AButton layers with a dropped
# parent-version field (the client reads name bytes as the version -
# "Wrong res version (1 != 28484)" -> MenuGrid PaginaException on world
# entry). Repair them in place; idempotent, no-op when the pack is
# already consistent.
python (Join-Path $root "server\scripts\fix_gameres_versions.py") $out

# Second pass: stale parent_ver REFERENCES (the version bytes are
# present but predate the parent file's real version - e.g. string.res
# referencing paginae/craft/clothmat ver 1 while the shipped file is
# ver 3). A strict client requests the stale version over HTTP,
# rejects the served file, and MenuGrid throws PaginaException on
# world entry. Fix both the generated pack AND the res/compiled
# overlay source (the client loads fork paginae from it locally).
python (Join-Path $root "server\scripts\fix_gameres_parent_refs.py") $out
python (Join-Path $root "server\scripts\fix_gameres_parent_refs.py") (Join-Path $root "res\compiled") --using $out
