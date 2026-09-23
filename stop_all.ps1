# ============================================================================
#  停止由 start_all.ps1 启动的全部进程
# ============================================================================

param([switch]$Quiet)

$Root = Split-Path -Parent $MyInvocation.MyCommand.Path
$PidFile = Join-Path $Root '.running.pids'

# 回收策略（两级，避免漏杀）：
#   1) 优先按 .running.pids 精确回收（最安全，不误伤）
#   2) 若上一步没找到任何进程，回落到按命令行特征匹配
#      —— 覆盖「手工启动」「PID 文件过期」「外层 shell 被杀而 Python 子进程残留」等情况
function Get-EdgeProcesses {
    $rootEscaped = [regex]::Escape($Root)
    @(Get-CimInstance Win32_Process -Filter "Name='python.exe'" -ErrorAction SilentlyContinue |
        Where-Object {
            $_.CommandLine -match '(run\.py|agent\.py)' -and
            ($_.CommandLine -match $rootEscaped -or $_.CommandLine -match 'edge-cluster')
        })
}

$targets = @()
if (Test-Path $PidFile) {
    $ids = Get-Content $PidFile | Where-Object { $_ -match '^\d+$' } | ForEach-Object { [int]$_ }
    $targets = @(Get-CimInstance Win32_Process | Where-Object { $ids -contains $_.ProcessId })
    if ($targets.Count -gt 0) {
        if (-not $Quiet) { Write-Host "按 .running.pids 找到 $($targets.Count) 个进程" }
    } else {
        if (-not $Quiet) { Write-Host '.running.pids 里的进程已全部退出，改用命令行特征回收' }
    }
} else {
    if (-not $Quiet) { Write-Host '没有找到 .running.pids，改用命令行特征回收' }
}
if ($targets.Count -eq 0) { $targets = Get-EdgeProcesses }

if (-not $targets -or $targets.Count -eq 0) {
    if (-not $Quiet) { Write-Host '没有正在运行的 edge-cluster 进程。' }
    Remove-Item $PidFile -ErrorAction SilentlyContinue
    return
}

foreach ($t in $targets) {
    try {
        Stop-Process -Id $t.ProcessId -Force -ErrorAction Stop
        if (-not $Quiet) { Write-Host "  已停止 PID=$($t.ProcessId)  $($t.Name)" }
    } catch {
        if (-not $Quiet) { Write-Warning "  停止 PID=$($t.ProcessId) 失败：$($_.Exception.Message)" }
    }
}

Remove-Item $PidFile -ErrorAction SilentlyContinue
if (-not $Quiet) { Write-Host "`n全部进程已停止。" -ForegroundColor Green }
