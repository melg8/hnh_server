@echo off
REM One-command start: resources -> save dir -> server (seed-fixed world).
REM Ports: 1871/tcp TLS auth, 1870/udp game, 1872/tcp resources HTTP.
setlocal enabledelayedexpansion
cd /d "%~dp0.."

REM 0. Current source revision: drives the staleness guards below and
REM    the HNH_REV log stamp (empty when git is unavailable).
set "GITREV="
for /f %%i in ('git rev-parse HEAD 2^>nul') do set "GITREV=%%i"

REM 1. Build. Always run cargo: it is incremental, so on an up-to-date
REM    source tree this is a sub-second no-op, but after a git pull it
REM    guarantees the binary matches the code instead of silently
REM    running a stale one.
call "%~dp0build-server.bat"
if errorlevel 1 exit /b 1

REM 2. Generate gameres\ from lib\haven-res.jar + res\compiled overlay.
REM    Stale-pack guard: regenerate when the pack is missing OR when HEAD
REM    moved since it was generated - a pack from an older tree silently
REM    misses newer resources and breaks the client avatar.
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
