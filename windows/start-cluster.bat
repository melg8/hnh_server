@echo off
REM Two-node cluster on one machine, one command.
REM Node 0: auth 1871 / game 1870 / res 1872 (client-facing defaults).
REM Node 1: auth 1873 / game 1872 is taken -> 1874 / res 1876...
REM Each node persists to its own shard: save\cluster_n0.json / cluster_n1.json.
REM A character created on either node migrates to whichever node the
REM client's session lands on (CharQuery/CharData over the node mesh).
setlocal enabledelayedexpansion
cd /d "%~dp0.."

set "GITREV="
for /f %%i in ('git rev-parse HEAD 2^>nul') do set "GITREV=%%i"

call "%~dp0build-server.bat"
if errorlevel 1 exit /b 1

REM Same stale-pack guard as start-server.bat: the client and both nodes
REM must serve a pack that matches the checked-out tree.
set "GENRES="
if not exist "gameres\gfx\borka\hair.res" set "GENRES=1"
set "GENREV="
if exist "gameres\.genrev" set /p GENREV=<"gameres\.genrev"
if not "%GITREV%"=="%GENREV%" set "GENRES=1"
if defined GENRES (
    echo Generating gameres resource pack ^(rev %GITREV%^)...
    powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0make-gameres.ps1"
    if errorlevel 1 (
        echo [ERROR] gameres generation failed.
        exit /b 1
    )
    if not "%GITREV%"=="" >"gameres\.genrev" echo %GITREV%
)

if not exist "save" mkdir "save"
if not exist "logs" mkdir "logs"

set "HNH_REV="
for /f %%i in ('git rev-parse --short HEAD 2^>nul') do set "HNH_REV=%%i"

set "NODES=127.0.0.1:18790,127.0.0.1:18791"

echo Starting cluster node 0 (auth 1871, game 1870, res 1872, mesh 18790)...
start "hnh node 0" cmd /c "set HNH_SAVE_FILE=save\cluster_n0.json&& server\target\release\hnh-server.exe --seed 42 --cluster %NODES% --node 0 >> logs\cluster-n0.log 2>&1"

echo Starting cluster node 1 (auth 1873, game 1874, res 1876, mesh 18791)...
start "hnh node 1" cmd /c "set HNH_SAVE_FILE=save\cluster_n1.json&& server\target\release\hnh-server.exe --seed 42 --cluster %NODES% --node 1 --game-port 1874 --auth-port 1873 --res-port 1876 >> logs\cluster-n1.log 2>&1"

echo.
echo Both nodes launched in their own windows. The plain client connects to
REM node 0's client-facing ports (run-client.bat). To log in through node 1,
REM start the client with -Dhaven.authserv=127.0.0.1:1873 (AuthClient takes
REM host:port; the client then plays on node 1's game port).
echo Logs: logs\cluster-n0.log / logs\cluster-n1.log (append).
echo Shard saves: save\cluster_n0.json / save\cluster_n1.json.
endlocal
