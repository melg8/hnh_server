@echo off
REM Bundle server + client logs and environment info into one zip file.
REM Send that zip when reporting a problem. Safe to run while the server
REM is up - log files are copied even while the server writes to them.
powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0collect-logs.ps1" -RepoRoot "%~dp0.."
pause
