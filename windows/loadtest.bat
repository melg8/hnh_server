@echo off
REM Load test demo: saturated world + 600 in-process bots through the REAL
REM UDP path. Bots walk, fight and interact; --perf prints a 5-second
REM performance report (tick_us must stay well below 100000 = 100 ms).
REM Press Ctrl+C to stop.
setlocal
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

echo Load test: 600 bots, saturated wildlife, live performance report...
echo (On 4-8 GB machines use 300 bots; 1000+ needs ~2 GB free RAM.)
server\target\release\hnh-server.exe --seed 42 --bots 600 --saturated --workers 4 --perf %*
endlocal
