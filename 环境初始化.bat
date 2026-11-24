@echo off
chcp 65001 >nul
echo Edge Cluster - environment setup (will request admin)...
echo.
powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0环境初始化.ps1" %*
echo.
pause
