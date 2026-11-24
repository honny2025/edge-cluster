# 环境初始化 —— 在新电脑上跑一次，把「只跑不改代码」所需的环境补齐。
#
# 会做三件事（全部幂等：重复跑不会重复安装、不会重复建防火墙规则）：
#   1) 检查 / 安装 WebView2 运行时（桌面壳依赖，Win10/11 一般已自带）
#   2) 放行防火墙 TCP 8000（让手机能连上平台；本机自用可跳过）
#   3) 检查 Universal CRT（Win10/11 内置，正常不用管）
#
# 用法：
#   双击同目录的「环境初始化.bat」（会自动提权），
#   或右键本脚本 →「使用 PowerShell 运行」。
#   本机自用（不让手机连）可加参数 -SkipFirewall 跳过防火墙那步。

param([switch]$SkipFirewall)

$ErrorActionPreference = 'Continue'

function Step([string]$m) { Write-Host ""; Write-Host "==> $m" -ForegroundColor Cyan }
function Ok([string]$m)   { Write-Host "    [OK] $m" -ForegroundColor Green }
function Warn([string]$m) { Write-Host "    [!!] $m" -ForegroundColor Yellow }

# ---- 自提权（防火墙规则需要管理员）----
$isAdmin = ([Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()).IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
if (-not $isAdmin) {
    Write-Host "需要管理员权限（要写防火墙规则），正在尝试提权重新运行…" -ForegroundColor Yellow
    $argList = "-NoProfile -ExecutionPolicy Bypass -File `"$PSCommandPath`""
    if ($SkipFirewall) { $argList += " -SkipFirewall" }
    Start-Process -FilePath "powershell.exe" -ArgumentList $argList -Verb RunAs
    exit
}

# ---- 1) WebView2 运行时 ----
Step "1/3 WebView2 运行时"
$wv2Found = $false
foreach ($p in @(
        "$env:ProgramFiles(x86)\Microsoft\EdgeWebView\Application",
        "$env:ProgramFiles\Microsoft\EdgeWebView\Application"
    )) {
    if (Test-Path $p) { $wv2Found = $true; break }
}
if ($wv2Found) {
    Ok "已安装"
} else {
    Warn "未检测到 WebView2 运行时，开始静默下载安装（Evergreen x64）…"
    $inst = Join-Path $env:TEMP "MicrosoftEdgeWebView2RuntimeInstallerX64.exe"
    try {
        Invoke-WebRequest -Uri "https://go.microsoft.com/fwlink/p/?LinkId=2124703" -OutFile $inst -UseBasicParsing
        Start-Process -FilePath $inst -ArgumentList "/install /silent" -Wait
        Ok "WebView2 运行时安装完成"
    } catch {
        Warn "下载 / 安装失败：$($_.Exception.Message)"
        Warn "请手动安装：https://developer.microsoft.com/microsoft-edge/webview2/ （选 Evergreen Standalone x64）"
    }
}

# ---- 2) 防火墙 TCP 8000 ----
if ($SkipFirewall) {
    Step "2/3 防火墙（已跳过 -SkipFirewall）"
} else {
    Step "2/3 防火墙 TCP 8000"
    $rule = Get-NetFirewallRule -DisplayName "edge-cluster 8000" -ErrorAction SilentlyContinue
    if ($rule) {
        Ok "规则已存在，跳过"
    } else {
        New-NetFirewallRule -DisplayName "edge-cluster 8000" -Direction Inbound `
            -Protocol TCP -LocalPort 8000 -Action Allow -Profile Private,Domain | Out-Null
        Ok "已放行 TCP 8000（手机连平台用）"
    }
    Write-Host "    （注：UDP 37020 是平台主动广播发现手机用的；当前架构是手机连平台，无需放行）" -ForegroundColor DarkGray
}

# ---- 3) Universal CRT ----
Step "3/3 Universal CRT"
if (Test-Path "$env:SystemRoot\System32\ucrtbase.dll") {
    Ok "UCRT 已就绪（Win10/11 内置）"
} else {
    Warn "未找到 ucrtbase.dll —— 系统可能过旧，请更新 Windows 或安装 VC++ 运行库"
}

# ---- 完成 ----
Step "完成"
Write-Host "现在可以双击「启动.bat」或「edge-cluster-desktop.exe」启动平台了。" -ForegroundColor Green
Write-Host "控制台地址：http://127.0.0.1:8000" -ForegroundColor Green
