@echo off
REM One-command start: resources -> save dir -> server (seed-fixed world).
REM Ports: 1871/tcp TLS auth, 1870/udp game, 1872/tcp resources HTTP.
setlocal enabledelayedexpansion
cd /d "%~dp0.."

REM 1. Build. Always run cargo: it is incremental, so on an up-to-date
REM    source tree this is a sub-second no-op, but after a git pull it
REM    guarantees the binary matches the code instead of silently
REM    running a stale one.
call "%~dp0build-server.bat"
if errorlevel 1 exit /b 1

REM 2. Generate gameres\ from lib\haven-res.jar + res\compiled overlay.
REM    The hair.res probe regenerates stale packs that predate the base
REM    hair/head aliases (missing ones break the client avatar).
if not exist "gameres\gfx\borka\hair.res" (
    echo Generating gameres resource pack...
    powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0make-gameres.ps1"
    if errorlevel 1 (
        echo [ERROR] gameres generation failed.
        exit /b 1
    )
)

REM 3. Ensure the save directory exists.
if not exist "save" mkdir "save"

REM 4. Run. Extra args pass through, e.g.: start-server.bat --bots 300 --perf
REM    HNH_REV stamps every server log line with the exact source revision,
REM    which makes bug reports reproducible.
set "HNH_REV="
for /f %%i in ('git rev-parse --short HEAD 2^>nul') do set "HNH_REV=%%i"
echo Starting hnh-server (seed 42)...
echo   auth: tcp/1871  game: udp/1870  resources: tcp/1872
server\target\release\hnh-server.exe --seed 42 %*
endlocal
