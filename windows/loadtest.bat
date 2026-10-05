@echo off
REM Load test demo: saturated world + a full bot cohort through the REAL UDP
REM path. Bots walk, fight the wildlife, harvest trees/stones and pick up the
REM drops; --perf prints a 5-second performance report (tick_us must stay
REM well below 100000 = 100 ms). Press Ctrl+C to stop.
REM Usage: loadtest.bat [bots] (default 1000)
setlocal
set BOT_COUNT=%1
if "%BOT_COUNT%"=="" set BOT_COUNT=1000
cd /d "%~dp0.."

if not exist "server\target\release\hnh-server.exe" (
    echo Binary missing, building...
    call "%~dp0build-server.bat"
    if errorlevel 1 exit /b 1
)
if not exist "gameres" (
    powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0make-gameres.ps1"
)
if not exist "save" mkdir "save"

echo Load test: %BOT_COUNT% bots (walk/fight/harvest/loot), saturated wildlife, live report...
echo (On 4-8 GB machines use 300-600 bots; 1000 bots need ~2 GB free RAM.)
server\target\release\hnh-server.exe --seed 42 --bots %BOT_COUNT% --bot-secs 600 --saturated --workers 4 --perf %2 %3 %4
endlocal
