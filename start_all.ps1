# ============================================================================
#  PC 端一键启动（Windows PowerShell，Rust 版）
#
#  用法：
#    .\start_all.ps1                      # 起平台 + 2 个模拟节点
#    .\start_all.ps1 -Nodes 3             # 起 3 个模拟节点
#    .\start_all.ps1 -BuildWeb            # 先构建前端（单端口交付）
#    .\start_all.ps1 -NoNodes             # 只起平台
#    .\start_all.ps1 -DevWeb              # 额外起 Vite 开发服务器（热更新）
#    .\start_all.ps1 -Port 9000           # 换平台端口（默认读 config.toml，即 8000）
#
#  ★ 可移植性说明（本脚本刻意做成「换一台电脑也能一样跑」）：
#   * v0.5.0-m5 起后端整体用 Rust 重写，本脚本拉起的是编译出的 Rust 二进制
#     （server-rust 的 edge-cluster-server、node-agent-rust 的 edge-cluster-agent），
#     不再依赖 Python / venv / pip。交付包已带编译好的二进制，无需任何工具链；
#     只有改源码重新编译才需要 Rust 工具链（cargo）。
#   * 二进制**自动探测**：target\release > target\debug > 目录平铺；
#     找不到时自动 `cargo build --release`（首次较慢，之后秒过）。
#   * 启动前检查端口占用，被占用时给出明确提示，而不是静默失败。
#   * 不用 Start-Process 的 -RedirectStandard* 参数（见 Start-Detached 注释）。
#   * 若本机设置了 HTTP_PROXY，脚本会为本地直连清空代理变量，
#     否则 Agent 连 127.0.0.1 会被代理拒绝。
# ============================================================================

param(
    [int]$Nodes = 2,
    [switch]$NoNodes,
    [switch]$BuildWeb,
    [switch]$DevWeb,
    [int]$Port = 0
)

# 让中文在控制台正确显示（不同版本 PowerShell 默认编码不一致，会导致乱码）
try {
    [Console]::OutputEncoding = [System.Text.Encoding]::UTF8
    $OutputEncoding = [System.Text.Encoding]::UTF8
} catch { }

$ErrorActionPreference = 'Stop'
$Root = Split-Path -Parent $MyInvocation.MyCommand.Path
$ServerDir = Join-Path $Root 'server-rust'
$AgentDir = Join-Path $Root 'node-agent-rust'
$WebDir = Join-Path $Root 'web'
$ServerBinName = 'edge-cluster-server'
$AgentBinName = 'edge-cluster-agent'

# ---------------------------------------------------------------- 平台端口探测
# 优先命令行 -Port，其次读 server-rust/config.toml，最后默认 8000
function Get-PlatformPort {
    param([int]$Override)
    if ($Override -gt 0) { return $Override }
    $cfg = Join-Path $Root 'server-rust\config.toml'
    if (Test-Path $cfg) {
        $hit = Select-String -Path $cfg -Pattern '^\s*port\s*=\s*(\d+)' -ErrorAction SilentlyContinue |
               Select-Object -First 1
        if ($hit) { return [int]$hit.Matches[0].Groups[1].Value }
    }
    return 8000
}

$PlatformPort = Get-PlatformPort -Override $Port
$PlatformUrl = "http://127.0.0.1:$PlatformPort"

function Test-PortInUse {
    param([int]$P)
    $conn = @(Get-NetTCPConnection -LocalPort $P -State Listen -ErrorAction SilentlyContinue)
    return ($conn.Count -gt 0)
}

function Write-Step($msg) { Write-Host "`n>>> $msg" -ForegroundColor Cyan }

# ---------------------------------------------------------------- Rust 二进制探测
# 依次尝试 release / debug / 平铺三种布局，兼容开发态与发布态。
function Get-RustBinary {
    param([Parameter(Mandatory = $true)][string]$Dir,
          [Parameter(Mandatory = $true)][string]$Name)
    $exe = $Name
    if ($env:OS -eq 'Windows_NT') { $exe = "$Name.exe" }
    foreach ($rel in @("target\release\$exe", "target\debug\$exe", $exe)) {
        $cand = Join-Path $Dir $rel
        if (Test-Path $cand) { return $cand }
    }
    return $null
}

# 找到二进制；找不到就编译。返回完整路径。
function Ensure-RustBinary {
    param([Parameter(Mandatory = $true)][string]$Dir,
          [Parameter(Mandatory = $true)][string]$Name,
          [Parameter(Mandatory = $true)][string]$Label)
    $bin = Get-RustBinary -Dir $Dir -Name $Name
    if ($bin) { return $bin }
    Write-Host "  未找到 $Label 二进制，正在编译（首次较慢，之后秒过）..." -ForegroundColor Yellow
    # 注意：GNU 工具链（stable-x86_64-pc-windows-gnu）需要 MinGW 的链接器在 PATH 上，
    # 例如 `export PATH=/d/MinGW64-16.2.0/bin:$PATH`。MSVC 工具链无需额外配置。
    Push-Location $Dir
    try {
        cargo build --release
        if ($LASTEXITCODE -ne 0) { throw "$Label 编译失败（exit $LASTEXITCODE）。请确认 cargo 已安装且工具链完整。" }
    } finally { Pop-Location }
    $bin = Get-RustBinary -Dir $Dir -Name $Name
    if (-not $bin) { throw "$Label 编译后仍未找到二进制。" }
    return $bin
}

# 探测 Node.js 目录（构建前端 / 起 Vite dev 用）。可移植，不写死路径。
function Resolve-NodeDir {
    if ($env:EDGE_NODE) {
        if (Test-Path $env:EDGE_NODE) { return $env:EDGE_NODE }
        throw "EDGE_NODE 指向的路径不存在：$env:EDGE_NODE"
    }
    $base = Join-Path $env:USERPROFILE '.workbuddy\binaries\node\versions'
    if (Test-Path $base) {
        $hit = Get-ChildItem $base -Directory -ErrorAction SilentlyContinue |
            Where-Object { Test-Path (Join-Path $_.FullName 'node.exe') } |
            Sort-Object Name -Descending | Select-Object -First 1
        if ($hit) { return $hit.FullName }
    }
    $c = Get-Command node -ErrorAction SilentlyContinue
    if ($c) { return (Split-Path -Parent $c.Source) }
    return $null
}

# ---------------------------------------------------------------- 后台启动助手
# 为什么不用 Start-Process -RedirectStandardOutput/-RedirectStandardError：
#   在「环境里同时存在 Path 和 PATH 两个键」的机器上（装了 Git / Node / Cygwin，
#   或跑在某些自动化宿主里时很常见），PowerShell 5.1 会抛
#       "已添加项。字典中的关键字:Path 所添加的关键字:PATH"
#   报错还会被包装成更难懂的"找不到接受实际参数 Path 的位置形式参数"，
#   导致平台完全起不来。这两个参数会强制 UseShellExecute=false，从而踩中该缺陷。
#
#   而"把控制台输出重定向"这件事对这个项目本来就不必要：
#   平台和 Agent 都自带文件日志（server-rust/data/logs/ 与 node-agent-rust/logs/）。
#   所以这里直接用窗口隐藏方式启动，既绕开缺陷，又保证进程完全脱离父窗口
#   （关掉启动脚本的窗口不会带走服务）。
function Start-Detached {
    param(
        [Parameter(Mandatory = $true)][string]$Exe,
        [string[]]$Arguments = @(),
        [string]$WorkingDir
    )
    $p = Start-Process -FilePath $Exe -ArgumentList $Arguments `
        -WorkingDirectory $WorkingDir -PassThru -WindowStyle Hidden
    return [int]$p.Id
}

# ---------------------------------------------------------------- 前端
# 本地直连必须绕过系统代理，否则 WS 连接会被代理以 502 拒绝
$env:NO_PROXY = '127.0.0.1,localhost,10.0.0.0/8,192.168.0.0/16,172.16.0.0/12'
$env:no_proxy = $env:NO_PROXY

if ($BuildWeb) {
    Write-Step '构建前端（web/dist）'
    $nd = Resolve-NodeDir
    if (-not $nd) {
        throw "构建前端需要 Node.js 18+。请先安装 Node，或用环境变量 EDGE_NODE 指定 node 所在目录。"
    }
    $npm = Join-Path $nd 'npm.cmd'
    if (-not (Test-Path $npm)) { $npm = 'npm' }
    Push-Location $WebDir
    try {
        if (-not (Test-Path 'node_modules')) { & $npm install --no-audit --no-fund }
        & $npm run build
    } finally { Pop-Location }
}

# ---------------------------------------------------------------- 平台
Write-Step "启动平台服务端（端口 $PlatformPort）"

# 端口预检：被占用时给出可执行的下一步，而不是等 20 秒后一句"未就绪"
if (Test-PortInUse $PlatformPort) {
    Write-Warning "端口 $PlatformPort 已被占用，平台起不来。"
    Write-Host '  如果平台其实已经在跑，直接打开即可：' -ForegroundColor Yellow
    Write-Host "    $PlatformUrl"
    Write-Host '  想换端口的话，用 -Port 指定（节点会自动跟着走，无需改配置）：' -ForegroundColor Yellow
    Write-Host '    .\start_all.ps1 -Port 8001'
    exit 1
}

$serverBin = Ensure-RustBinary -Dir $ServerDir -Name $ServerBinName -Label '服务端'
New-Item -ItemType Directory -Force -Path (Join-Path $ServerDir 'data\logs') | Out-Null
# Rust 服务端不支持 --port 参数，端口走 EDGE_SERVER__PORT 环境变量（优先级高于 config.toml）。
# 显式设置，保证实际监听端口与脚本探测/健康检查用的端口一致。
$env:EDGE_SERVER__PORT = "$PlatformPort"
$serverPid = Start-Detached -Exe $serverBin -WorkingDir $ServerDir
Write-Host "  平台进程 PID=$serverPid"
Write-Host '  等待平台就绪...'

$ready = $false
foreach ($i in 1..30) {
    Start-Sleep -Milliseconds 700
    try {
        $r = Invoke-RestMethod -Uri "$PlatformUrl/api/v1/system/health" -TimeoutSec 2
        if ($r.status -eq 'ok') { $ready = $true; break }
    } catch { }
}
if ($ready) {
    Write-Host "  平台已就绪：$PlatformUrl  (接口文档 $PlatformUrl/docs)" -ForegroundColor Green
} else {
    Write-Warning '平台未在预期时间内就绪。'
    Write-Host '  排查办法：前台手工运行一次，就能看到具体报错 ——' -ForegroundColor Yellow
    Write-Host "    cd `"$ServerDir`""
    Write-Host "    `"$serverBin`""
}

# ---------------------------------------------------------------- 模拟节点
$agentPids = @()
if (-not $NoNodes) {
    $agentBin = Ensure-RustBinary -Dir $AgentDir -Name $AgentBinName -Label 'Agent'
    Write-Step "启动 $Nodes 个模拟节点"
    for ($i = 1; $i -le $Nodes; $i++) {
        $name = 'phone-{0:d2}' -f $i
        New-Item -ItemType Directory -Force -Path (Join-Path $AgentDir 'logs') | Out-Null
        $nodePid = Start-Detached -Exe $agentBin `
            -Arguments @('--simulate', '--name', $name,
                         '--platform-url', $PlatformUrl) `
            -WorkingDir $AgentDir
        $agentPids += $nodePid
        Write-Host "  $name 已启动 PID=$nodePid"
        Start-Sleep -Milliseconds 400
    }
}

# ---------------------------------------------------------------- Vite dev
$vitePid = 0
if ($DevWeb) {
    Write-Step '启动 Vite 开发服务器（5173）'
    $nd = Resolve-NodeDir
    if (-not $nd) {
        throw "启动前端开发服务器需要 Node.js 18+。请先安装 Node，或用环境变量 EDGE_NODE 指定 node 所在目录。"
    }
    $npm = Join-Path $nd 'npm.cmd'
    if (-not (Test-Path $npm)) { $npm = 'npm' }
    $viteProc = Start-Process -FilePath 'cmd.exe' -ArgumentList '/c', "`"$npm`" run dev" `
        -WorkingDirectory $WebDir -PassThru -WindowStyle Hidden
    $vitePid = $viteProc.Id
    Write-Host "  Vite PID=$vitePid  http://127.0.0.1:5173"
}

# ---------------------------------------------------------------- 汇总
$pids = @($serverPid) + $agentPids
if ($vitePid -gt 0) { $pids += $vitePid }
$pids | Where-Object { $_ -gt 0 } | Set-Content -Path (Join-Path $Root '.running.pids') -Encoding utf8

Write-Host ''
# 注意：必须写成 ('=' * 68) —— Write-Host '=' * 68 会被当成参数列表，
# 实际打印出 "= * 68" 而不是一条分隔线
Write-Host ('=' * 68) -ForegroundColor Cyan
Write-Host ' 启动完成' -ForegroundColor Green
Write-Host ('=' * 68) -ForegroundColor Cyan
Write-Host ' 平台地址   : ' -NoNewline; Write-Host $PlatformUrl -ForegroundColor White
Write-Host ' 接口文档   : ' -NoNewline; Write-Host "$PlatformUrl/docs" -ForegroundColor White
if ($BuildWeb) { Write-Host " 控制台     : $PlatformUrl   （已托管前端构建产物）" }
if ($DevWeb)   { Write-Host ' 控制台(dev): http://127.0.0.1:5173' }
Write-Host ' 共享 token : edge-cluster-dev-token'
if (-not $NoNodes) { Write-Host " 模拟节点   : $Nodes 个（phone-01 ...）" }
Write-Host ''
Write-Host ' 停止全部   : .\stop_all.ps1'
if ($PlatformPort -eq 8000) {
    Write-Host ' 验收检查   : .venv\Scripts\python.exe docs\acceptance\m1_check.py --phase online'
} else {
    Write-Host " 验收检查   : .venv\Scripts\python.exe docs\acceptance\m1_check.py --base $PlatformUrl --phase online"
}
Write-Host ''
Write-Host ' 提示：进程已写入 .running.pids，可用 stop_all.ps1 统一回收。'
