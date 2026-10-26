# 反色灵敏度与显示配色验证：以 CSV（场景时间轴）+ 应用日志（bg flip）为契约，取点逻辑复用 measure_bg_lum.ps1
# 用法：powershell -NoProfile -ExecutionPolicy Bypass -File scripts\verify_invert.ps1 [-SceneSeconds 15]
#   用例 A（自动反色）：cycle 一轮恰 2 条 bg flip——bright 首秀判亮、切入 dark 的翻转不晚于 500ms，
#                       subtitle / letterbox 各 0 条（总数 2 即已保证）
#   用例 B（常驻白 / 常驻黑）：bright 场景全程 0 条 bg flip（固定色模式不采样、不读屏）
# 前置：cargo build --release；须在可交互桌面执行（脚本会弹全屏窗口并多次重启 timestick.exe）
param([int]$SceneSeconds = 15)
$ErrorActionPreference = "Stop"

$root = Split-Path $PSScriptRoot -Parent
$exe = Join-Path $root "target\release\timestick.exe" # 从脚本位置推导，不硬编码绝对路径
$settings = "$env:APPDATA\TimeStick\settings.txt"
$settingsBak = "$settings.bak"
$position = "$env:APPDATA\TimeStick\position.txt"
$positionBak = "$position.bak"
$measure = Join-Path $PSScriptRoot "measure_bg_lum.ps1"
$log = "$env:TEMP\timestick_invert.log"
$csv = "$env:TEMP\timestick_invert.csv"
# cycle 四场景各 SceneSeconds 秒：整段恰好一轮；多跑会进第二轮 bright，多出两次翻转污染计数
$cycleSeconds = 4 * $SceneSeconds
# 首条 bg flip 早于 CSV 首样本的余量：悬浮窗先出现再开始落 CSV，首采翻转发生在采样启动之前
$flipWindowMs = 5000
$fails = @()

if (-not (Test-Path $exe)) { Write-Host "FAIL: 未找到 $exe（先 cargo build --release）"; exit 1 }
if (-not (Test-Path $measure)) { Write-Host "FAIL: 未找到 $measure"; exit 1 }

# 结束应用：单实例互斥体需要时间释放，下一轮才能重新起进程
function Stop-App {
    Stop-Process -Name timestick -Force -ErrorAction SilentlyContinue
    Start-Sleep -Milliseconds 500
}

# 备份只做一次：多轮用例共用同一份原件，finally 才能还原到运行前内容
function Backup-Once([string]$file, [string]$bak) {
    if ((Test-Path $file) -and -not (Test-Path $bak)) { Copy-Item $file $bak -Force }
}

# 跑一轮场景：重启应用（日志从零起算）→ 子进程跑 measure → 收掉应用；返回 measure 退出码
function Invoke-Scene([string]$scene, [int]$seconds) {
    Stop-App
    # 日志与 CSV 均追加写，旧内容会污染计数；-WaitSeconds 60 让失败的用例快速收场（默认 300s 干等 5 分钟）
    Remove-Item $log, $csv -ErrorAction SilentlyContinue
    $env:TIMESTICK_LOG = $log
    Start-Process -FilePath $exe | Out-Null
    # 必须走子进程：measure 内 Add-Type BgSample 在同一进程二次执行会因类型已存在而报错；
    # -File 下 throw 与 exit 1 都表现为非零 $LASTEXITCODE；Out-Host 让子进程输出不进函数返回值
    & powershell.exe -NoProfile -ExecutionPolicy Bypass -File $measure -Scene $scene -Seconds $seconds `
        -SceneSeconds $SceneSeconds -Out $csv -WaitSeconds 60 | Out-Host
    $code = $LASTEXITCODE
    Stop-App
    return $code
}

# CSV 样本行 → 标签 + Unix 毫秒（time 列为本地 HH:mm:ss.fff，跨午夜时日期 +1）
function Read-Samples([string]$path) {
    if (-not (Test-Path $path)) { return @() }
    $rows = @()
    $baseDate = [DateTime]::Today
    $prevTod = $null
    foreach ($line in (Get-Content $path)) {
        if (-not $line -or $line -match '^#' -or $line -like 'time,*') { continue }
        $cols = $line -split ','
        $tod = [DateTime]::ParseExact($cols[0], 'HH:mm:ss.fff', $null)
        if ($null -ne $prevTod -and $tod -lt $prevTod) { $baseDate = $baseDate.AddDays(1) }
        $prevTod = $tod
        $rows += [pscustomobject]@{
            Label  = $cols[1]
            UnixMs = [DateTimeOffset]::new($baseDate + $tod.TimeOfDay, [DateTimeOffset]::Now.Offset).ToUnixTimeMilliseconds()
        }
    }
    return $rows
}

# 应用日志 → bg flip 清单（只取采样窗口内的：窗口外的翻转属收尾时的真实环境变化，不参与断言）
function Read-Flips([string]$path, [long]$from, [long]$to) {
    if (-not (Test-Path $path)) { return @() }
    $list = @()
    foreach ($line in (Get-Content $path)) {
        if ($line -match '^\[(\d+)\] bg flip: (?:lum=\S+ |sample failed )?dark=(true|false)$') {
            $ms = [long]$Matches[1]
            if ($ms -ge $from -and $ms -le $to) {
                $list += [pscustomobject]@{ UnixMs = $ms; Dark = ($Matches[2] -eq 'true') }
            }
        }
    }
    return $list
}

# 预检：残留实例（其他会话 / 无权结束的进程）会占住单实例互斥体，新进程静默退出且不写日志，
# 后面的用例只会得到“等悬浮窗超时”的假象，故先确认能独占
Stop-App
$leftover = @(Get-Process timestick -ErrorAction SilentlyContinue)
if ($leftover.Count -ne 0) {
    Write-Host ("FAIL: 仍有 timestick 实例在运行（PID {0}），请先结束它（脚本需要独占单实例互斥体）" -f (($leftover | ForEach-Object Id) -join ", "))
    exit 1
}

try {
    # ============ 用例 A：自动反色 ============
    Write-Host "`n=== 用例 A：自动反色 / cycle / $cycleSeconds s（每场 $SceneSeconds s）==="
    # 固定色模式会停掉采样定时器：上次会话若留下 color_mode=white|black，本用例会因"不采样"而空转
    Backup-Once $settings $settingsBak
    New-Item -ItemType Directory -Force -Path (Split-Path $settings -Parent) | Out-Null
    Set-Content -Path $settings -Value "color_mode=auto" -Encoding ASCII
    # letterbox 黑边只占上下各 22.5%：悬浮窗居中会采到中部银灰画面（192 > 160 判亮），打破 0 翻转预期
    Backup-Once $position $positionBak
    Remove-Item $position -ErrorAction SilentlyContinue

    $code = Invoke-Scene "cycle" $cycleSeconds
    if ($code -ne 0) {
        $fails += "A: measure_bg_lum 退出码 $code（无有效样本或等悬浮窗超时），翻转断言未执行"
    } elseif (-not (Test-Path $log)) { $fails += "A: 日志未生成 $log" }
    elseif (-not (Test-Path $csv)) { $fails += "A: CSV 未生成 $csv" }
    else {
        $samples = @(Read-Samples $csv)
        if ($samples.Count -eq 0) {
            $fails += "A: CSV 无样本行 $csv"
        } else {
            # 场景时间轴只取标签变化点（场景切换 → xxx 是控制台打印、无时间戳，不作解析依据）
            $anchors = @(for ($i = 0; $i -lt $samples.Count; $i++) {
                if ($i -eq 0 -or $samples[$i].Label -ne $samples[$i - 1].Label) { $samples[$i] }
            })
            $anchors | ForEach-Object { Write-Host "  场景 $($_.Label) @ $($_.UnixMs)" }
            $flips = @(Read-Flips $log ($samples[0].UnixMs - $flipWindowMs) $samples[$samples.Count - 1].UnixMs)
            $flips | ForEach-Object { Write-Host "  bg flip @ $($_.UnixMs) dark=$($_.Dark)" }
            Write-Host "  翻转条数 $($flips.Count)（期望 2）"

            if ($flips.Count -ne 2) {
                $fails += "A: bg flip 应为 2 条（bright 首秀判亮 + 切入 dark 判暗），实际 $($flips.Count) 条"
            }
            $darkAnchor = @($anchors | Where-Object { $_.Label -eq 'dark' } | Select-Object -First 1)
            if ($darkAnchor.Count -eq 0) {
                $fails += "A: CSV 未记录 dark 场景，无法校验切换时序"
            } else {
                $darkAt = $darkAnchor[0].UnixMs
                if ($flips.Count -ge 1) {
                    if ($flips[0].Dark) {
                        $fails += "A: 首条 bg flip 应判亮（dark=false），实际 dark=true"
                    } elseif ($flips[0].UnixMs -ge $darkAt) {
                        $fails += "A: 首条 bg flip 应早于 dark 段（bright 段进程首秀），实际 $($flips[0].UnixMs) >= $darkAt"
                    }
                }
                if ($flips.Count -ge 2) {
                    $delta = $flips[1].UnixMs - $darkAt
                    if (-not $flips[1].Dark) {
                        $fails += "A: 次条 bg flip 应判暗（dark=true），实际 dark=false"
                    } elseif ($delta -lt -300 -or $delta -gt 500) {
                        # 上界即 ≤500ms 翻转目标；下界 300ms 容纳"画面切换与 CSV 行落盘之间的毫秒级窗口"
                        $fails += "A: 切入 dark 的翻转偏移 ${delta}ms 超出 [-300, 500]"
                    } else {
                        Write-Host "  切入 dark 的翻转偏移 ${delta}ms（目标 ≤500ms）"
                    }
                }
            }
        }
    }

    # ============ 用例 B：常驻白 / 常驻黑 ============
    foreach ($mode in @("white", "black")) {
        $cn = if ($mode -eq "white") { "白" } else { "黑" }
        Write-Host "`n=== 用例 B：常驻$cn / bright / 20 s ==="
        Set-Content -Path $settings -Value "color_mode=$mode" -Encoding ASCII
        $code = Invoke-Scene "bright" 20
        if ($code -ne 0) {
            $fails += "B-${mode}: measure_bg_lum 退出码 $code（无有效样本或等悬浮窗超时），翻转断言未执行"
        } elseif (-not (Test-Path $log)) { $fails += "B-${mode}: 日志未生成 $log" }
        elseif (-not (Test-Path $csv)) { $fails += "B-${mode}: CSV 未生成 $csv" }
        else {
            $samples = @(Read-Samples $csv)
            if ($samples.Count -eq 0) {
                $fails += "B-${mode}: CSV 无样本行 $csv"
            } else {
                $flips = @(Read-Flips $log ($samples[0].UnixMs - $flipWindowMs) $samples[$samples.Count - 1].UnixMs)
                # 固定色模式下 show() 既不首采也不起 ID_BG：任何 bg flip 都是"仍在读屏"的泄漏证据
                if ($flips.Count -ne 0) {
                    $fails += "B-${mode}: 常驻色不应采样，实际出现 $($flips.Count) 条 bg flip"
                } else {
                    Write-Host "  翻转条数 0（期望 0，采样定时器已停）"
                }
            }
        }
    }
} finally {
    Stop-App
    # settings.txt / position.txt 原本不存在时，把测试期间新生成的文件一并清掉
    foreach ($pair in @(@($settings, $settingsBak), @($position, $positionBak))) {
        $file, $bak = $pair
        if (Test-Path $bak) { Move-Item $bak $file -Force }
        elseif (Test-Path $file) { Remove-Item $file -Force }
    }
}

if ($fails.Count -eq 0) {
    Write-Host "`nPASS"
    Write-Host "证据：日志 $log / CSV $csv"
    exit 0
}
$fails | ForEach-Object { Write-Host "FAIL: $_" }
exit 1
