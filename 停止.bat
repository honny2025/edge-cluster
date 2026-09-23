@echo off
chcp 936 >nul
cd /d "%~dp0"
echo.
echo 正在停止全部 edge-cluster 进程...
echo.
powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0stop_all.ps1"
echo.
pause >nul
