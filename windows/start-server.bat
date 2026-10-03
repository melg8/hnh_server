@echo off
REM One-command start: resources -> save dir -> server (seed-fixed world).
REM Ports: 1871/tcp TLS auth, 1870/udp game, 1872/tcp resources HTTP.
setlocal enabledelayedexpansion
cd /d "%~dp0.."

REM 1. Build if the binary is missing.
if not exist "server\target\release\hnh-server.exe" (
    echo Binary missing, building...
    call "%~dp0build-server.bat"
    if errorlevel 1 exit /b 1
)

REM 2. Generate gameres\ from lib\haven-res.jar + res\compiled overlay.
if not exist "gameres" (
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
echo Starting hnh-server (seed 42)...
echo   auth: tcp/1871  game: udp/1870  resources: tcp/1872
server\target\release\hnh-server.exe --seed 42 %*
endlocal
