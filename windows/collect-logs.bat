@echo off
REM Bundle server + client logs and environment info into one zip file.
REM Send that zip when reporting a problem.
powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0collect-logs.ps1"
pause
