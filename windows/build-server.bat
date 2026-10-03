@echo off
REM Build the Rust game server (release profile).
REM Requirements: Rust 1.75+ (https://rustup.rs)
setlocal
cd /d "%~dp0..\server"
where cargo >nul 2>nul
if errorlevel 1 (
    echo [ERROR] cargo not found. Install Rust from https://rustup.rs and restart this shell.
    exit /b 1
)
echo Building hnh-server (release)...
cargo build --release
if errorlevel 1 exit /b 1
echo.
echo Build OK: server\target\release\hnh-server.exe
echo Start it with start-server.bat
endlocal
