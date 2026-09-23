# ============================================================================
#  PC 端一键启动（Windows PowerShell）
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
#   * 运行时**自动探测**，不写死任何本机路径：优先环境变量 EDGE_PYTHON / EDGE_NODE，
#     其次 WorkBuddy 托管运行时，最后系统 PATH；探测到的 Python 会校验版本 >= 3.10
#   * 虚拟环境建在项目内 .venv —— 整个 edge-cluster 目录可整体拷贝到别的机器
#   * pip 安装失败时自动改用清华镜像重试（国内网络更稳）
#   * 启动前检查端口占用，被占用时给出明确提示，而不是静默失败
#   * 不用 Start-Process 的 -RedirectStandard* 参数（见 Start-Detached 注释：
#     该参数在部分环境下会因 Path/PATH 重复键而直接抛错）
#   * 若本机设置了 HTTP_PROXY，脚本会为本地直连清空代理变量，
#     否则 Agent 连 127.0.0.1 会被代理拒绝
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
$ServerDir = Join-Path $Root 'server'
$AgentDir = Join-Path $Root 'node-agent'
$WebDir = Join-Path $Root 'web'

# ---------------------------------------------------------------- 平台端口探测
# 优先命令行 -Port，其次读 server/config.toml，最后默认 8000
function Get-PlatformPort {
    param([int]$Override)
    if ($Override -gt 0) { return $Override }
    $cfg = Join-Path $Root 'server\config.toml'
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

# ---------------------------------------------------------------- 运行时探测
# 可移植性：本脚本不写死任何本机路径，改为自动探测解释器。
# 可用环境变量显式指定：EDGE_PYTHON / EDGE_NODE

function Resolve-Python {
    if ($env:EDGE_PYTHON) {
        if (Test-Path $env:EDGE_PYTHON) { return $env:EDGE_PYTHON }
        throw "EDGE_PYTHON 指向的路径不存在：$env:EDGE_PYTHON"
    }
    # 1) 托管 Python（WorkBuddy 环境，取版本号最高的一个）
    $base = Join-Path $env:USERPROFILE '.workbuddy\binaries\python\versions'
    if (Test-Path $base) {
        $hit = Get-ChildItem $base -Directory -ErrorAction SilentlyContinue |
            Where-Object { Test-Path (Join-Path $_.FullName 'python.exe') } |
            ForEach-Object {
                $v = [version]'0.0'
                try { $v = [version]($_.Name -replace '[^0-9.]', '') } catch { }
                [pscustomobject]@{ Dir = $_.FullName; Ver = $v }
            } |
            Sort-Object Ver -Descending | Select-Object -First 1
        if ($hit) { return (Join-Path $hit.Dir 'python.exe') }
    }
    # 2) 系统 PATH
    foreach ($n in @('python', 'python3', 'py')) {
        $c = Get-Command $n -ErrorAction SilentlyContinue
        if ($c) { return $c.Source }
    }
    throw "找不到 Python 3.10+。请先安装 Python，或用环境变量 EDGE_PYTHON 指定解释器路径。"
}

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

# 校验 Python 版本：项目要求 3.10+（更早的版本会在类型注解处直接语法报错，
# 报错信息很难懂，所以这里提前拦住并给出明确提示）
function Assert-PythonVersion {
    param([Parameter(Mandatory = $true)][string]$Python)
    $v = & $Python -c "import sys; print('%d.%d' % sys.version_info[:2])" 2>$null
    if ($LASTEXITCODE -ne 0 -or -not $v) {
        throw "无法执行 Python：$Python"
    }
    if ([version]$v -lt [version]'3.10') {
        throw ("需要 Python 3.10 或更高版本，当前为 $v （$Python）。`n" +
               "  请升级 Python 后重试，或用环境变量 EDGE_PYTHON 指定其他解释器。")
    }
    return $v
}

# 安装依赖：默认源失败时自动改用清华镜像重试（国内网络更稳）
function Install-Reqs {
    param(
        [Parameter(Mandatory = $true)][string]$Python,
        [Parameter(Mandatory = $true)][string]$ReqFile,
        [string]$Label = 'Python'
    )
    # 某些电脑设置了失效的本机代理（例如 127.0.0.1:7897），会导致 pip
    # 在尚未访问 PyPI 前就连接失败。可用 EDGE_KEEP_PROXY=1 保留代理。
    $proxyBackup = @{}
    if ($env:EDGE_KEEP_PROXY -ne '1') {
        foreach ($n in 'HTTP_PROXY','HTTPS_PROXY','ALL_PROXY','http_proxy','https_proxy','all_proxy') {
            $proxyBackup[$n] = [Environment]::GetEnvironmentVariable($n)
            [Environment]::SetEnvironmentVariable($n, $null)
        }
    }
    try {
    & $Python -m pip install --quiet --disable-pip-version-check -r $ReqFile
    if ($LASTEXITCODE -eq 0) { return }

    Write-Host "  $Label 依赖安装失败，改用清华镜像重试..." -ForegroundColor Yellow
    & $Python -m pip install --quiet --disable-pip-version-check `
        -i 'https://pypi.tuna.tsinghua.edu.cn/simple' -r $ReqFile
    if ($LASTEXITCODE -ne 0) {
        throw ("$Label 依赖安装失败（默认源与清华镜像都试过了）。`n" +
               "  请检查网络后手工执行：`n" +
               "    & `"$Python`" -m pip install -r `"$ReqFile`"")
    }
    } finally {
        foreach ($n in $proxyBackup.Keys) { [Environment]::SetEnvironmentVariable($n, $proxyBackup[$n]) }
    }
}

function Write-Step($msg) { Write-Host "`n>>> $msg" -ForegroundColor Cyan }

# ---------------------------------------------------------------- 后台启动助手
# 为什么不用 Start-Process -RedirectStandardOutput/-RedirectStandardError：
#   在「环境里同时存在 Path 和 PATH 两个键」的机器上（装了 Git / Node / Cygwin，
#   或跑在某些自动化宿主里时很常见），PowerShell 5.1 会抛
#       "已添加项。字典中的关键字:Path 所添加的关键字:PATH"
#   报错还会被包装成更难懂的"找不到接受实际参数 Path 的位置形式参数"，
#   导致平台完全起不来。这两个参数会强制 UseShellExecute=false，从而踩中该缺陷。
#
#   而"把控制台输出重定向"这件事对这个项目本来就不必要：
#   平台和 Agent 都自带文件日志（server/data/logs/ 与 node-agent/logs/）。
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

# ---------------------------------------------------------------- 环境准备
# 虚拟环境放在项目内（.venv），随目录整体拷贝即可带走，不依赖任何外部路径
$VenvDir = Join-Path $Root '.venv'
$VenvPython = Join-Path $VenvDir 'Scripts\python.exe'

Write-Step '检查 Python 运行环境'

$VenvReady = $false
if (Test-Path $VenvPython) {
    # 已存在 .venv：先体检。它可能是从别的电脑拷过来的，指向一个本机不存在的
    # 基础解释器（venv 里的 python.exe 靠 pyvenv.cfg 的 home 找标准库），
    # 那种情况下 python 连启动都会失败（实测 exit code 103）。
    try {
        $venvVer = Assert-PythonVersion -Python $VenvPython
        Write-Host "  复用已有虚拟环境 .venv (Python v$venvVer)"
        $VenvReady = $true
    } catch {
        Write-Warning "  已有 .venv 不可用，将重建：$($_.Exception.Message)"
        # 用「改名留存」而不是递归删除：
        #   ① 改名是 O(1)，递归删几千个文件又慢又可能中途失败留下半残目录
        #   ② 改名即使目录里有文件被占用也能成功
        #   ③ 万一诊断错了，坏环境还在，可以自行回看
        $broken = Join-Path $Root ('.venv.broken-' + (Get-Date -Format 'yyyyMMdd-HHmmss'))
        try {
            Rename-Item -LiteralPath $VenvDir -NewName (Split-Path -Leaf $broken) -ErrorAction Stop
            Write-Host "  已把损坏的环境改名保留为：$(Split-Path -Leaf $broken)（确认没用后可自行删除）"
        } catch {
            Write-Warning "  改名失败，退回直接删除"
            Remove-Item -LiteralPath $VenvDir -Recurse -Force -ErrorAction SilentlyContinue
        }
    }
}

if (-not $VenvReady) {
    $BasePython = Resolve-Python
    $baseVer = Assert-PythonVersion -Python $BasePython
    Write-Host "  使用 Python：$BasePython (v$baseVer)"
    Write-Host "  创建隔离虚拟环境 $VenvDir"
    & $BasePython -m venv $VenvDir
    if (-not (Test-Path $VenvPython)) {
        throw "虚拟环境创建失败，未生成 $VenvPython。请确认 Python 安装完整（含 venv 模块）。"
    }
}

Write-Host '  安装/校验后端依赖（已装则秒过）'
Install-Reqs -Python $VenvPython -ReqFile (Join-Path $ServerDir 'requirements.txt') -Label '后端'
Write-Host '  安装/校验 Agent 依赖'
Install-Reqs -Python $VenvPython -ReqFile (Join-Path $AgentDir 'requirements.txt') -Label 'Agent'

# 本地直连必须绕过系统代理，否则 WS 连接会被代理以 502 拒绝
$env:NO_PROXY = '127.0.0.1,localhost,10.0.0.0/8,192.168.0.0/16,172.16.0.0/12'
$env:no_proxy = $env:NO_PROXY

# ---------------------------------------------------------------- 前端
if ($BuildWeb) {
    Write-Step '构建前端（web/dist）'
    $nd = Resolve-NodeDir
    if (-not $nd) {
        throw "构建前端需要 Node.js 18+。请先安装 Node，或用环境变量 EDGE_NODE 指定 node 所在目录。"
    }
    # 直接调用 node 目录下的 npm.cmd，**不修改 $env:PATH** ——
    # 给 $env:PATH 赋值会在某些环境下制造 "Path"/"PATH" 大小写重复键，
    # 进而让后面 Start-Process 的重定向抛"已添加项"错误（见 Start-Detached 注释）。
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

$serverLog = Join-Path $ServerDir 'data\logs'
New-Item -ItemType Directory -Force -Path $serverLog | Out-Null
# 显式传 --port：保证服务端实际监听端口与脚本探测/健康检查用的端口一致
$serverPid = Start-Detached -Exe $VenvPython `
    -Arguments @('run.py', '--port', "$PlatformPort") -WorkingDir $ServerDir
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
    Write-Host "    & `"$VenvPython`" run.py --port $PlatformPort"
}

# ---------------------------------------------------------------- 模拟节点
$agentPids = @()
if (-not $NoNodes) {
    Write-Step "启动 $Nodes 个模拟节点"
    for ($i = 1; $i -le $Nodes; $i++) {
        $name = 'phone-{0:d2}' -f $i
        New-Item -ItemType Directory -Force -Path (Join-Path $AgentDir 'logs') | Out-Null
        $nodePid = Start-Detached -Exe $VenvPython `
            -Arguments @('agent.py', '--simulate', '--name', $name,
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
    Write-Host ' 验收检查   : python docs\acceptance\m1_check.py --phase online'
} else {
    Write-Host " 验收检查   : python docs\acceptance\m1_check.py --base $PlatformUrl --phase online"
}
Write-Host ''
Write-Host ' 提示：进程已写入 .running.pids，可用 stop_all.ps1 统一回收。'
