@echo off
chcp 65001 >nul
setlocal

cd /d "%~dp0"

echo ============================================
echo   构建前端（让「电量 / NPU」两列生效）
echo ============================================
echo.

where node >nul 2>nul
if errorlevel 1 (
    echo [错误] 没找到 Node.js。
    echo 请先安装 Node.js（选 LTS 长期支持版）：
    echo   https://nodejs.org/
    echo 安装完成后，重新双击本脚本即可。
    echo.
    pause
    exit /b 1
)

if not exist "node_modules" (
    echo [1/2] 首次构建，正在安装依赖（npm install，需联网，可能要几分钟）...
    call npm install
    if errorlevel 1 (
        echo.
        echo [错误] 依赖安装失败，请检查网络后重试，或把报错截图发我。
        echo.
        pause
        exit /b 1
    )
) else (
    echo [1/2] 依赖已存在，跳过安装。
)

echo.
echo [2/2] 正在构建（npm run build）...
call npm run build
if errorlevel 1 (
    echo.
    echo [错误] 构建失败，请把上面的报错截图发我。
    echo.
    pause
    exit /b 1
)

echo.
echo [完成] 前端已重新构建。
echo 现在回到平台页面按 Ctrl+F5 强制刷新，即可看到「电量」「NPU」两列。
echo （若用桌面版 edge-cluster-desktop.exe，关闭窗口重新打开即可。）
echo.
pause
