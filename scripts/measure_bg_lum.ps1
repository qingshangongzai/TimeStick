# 背景亮度实测：复刻「上沿 5 点 + 左右各 2 点 + 低中位数」采样，逐样本落 CSV
# 用法：
#   1) 合成场景（推荐，可复现、无需素材、不抢前台）：脚本自己弹全屏画面
#      .\scripts\measure_bg_lum.ps1 -Scene bright -Seconds 120
#      .\scripts\measure_bg_lum.ps1 -Scene cycle  -Seconds 120   # 每 20s 轮换四种场景
#   2) 真实视频：先启动 timestick.exe 再运行本脚本，随后把播放器切全屏（脚本会等悬浮窗出现）
#      .\scripts\measure_bg_lum.ps1 -Label dark -Seconds 120
#   3) 手工区域（不依赖悬浮窗）：.\scripts\measure_bg_lum.ps1 -Rect 左,上,宽,高 -Seconds 10
param(
    [ValidateSet("", "bright", "dark", "subtitle", "letterbox", "cycle")][string]$Scene = "",
    [ValidateSet("bright", "dark", "subtitle", "letterbox", "misc")][string]$Label = "misc",
    [int]$Seconds = 60,
    [int]$TickMs = 250,
    [int]$WaitSeconds = 300,
    [int]$SceneSeconds = 20,
    [string]$Out = "",
    [string]$Rect = ""
)
$ErrorActionPreference = "Stop"

Add-Type @"
using System;
using System.Runtime.InteropServices;
using System.Text;
public class BgSample {
    [DllImport("user32.dll")] public static extern bool EnumWindows(EnumProc cb, IntPtr l);
    public delegate bool EnumProc(IntPtr h, IntPtr l);
    [DllImport("user32.dll")] public static extern int GetClassName(IntPtr h, StringBuilder sb, int max);
    [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
    [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
    [DllImport("user32.dll")] public static extern IntPtr GetDC(IntPtr h);
    [DllImport("user32.dll")] public static extern int ReleaseDC(IntPtr h, IntPtr dc);
    [DllImport("gdi32.dll")] public static extern uint GetPixel(IntPtr dc, int x, int y);
    [DllImport("user32.dll")] public static extern IntPtr SetThreadDpiAwarenessContext(IntPtr v);
    [DllImport("user32.dll")] public static extern int GetSystemMetrics(int i);
    public struct RECT { public int Left, Top, Right, Bottom; }

    public static IntPtr FindFloater() {
        IntPtr found = IntPtr.Zero;
        EnumWindows(delegate(IntPtr h, IntPtr l) {
            var sb = new StringBuilder(256);
            GetClassName(h, sb, 256);
            if (sb.ToString() == "TimeStickFloater" && IsWindowVisible(h)) { found = h; return false; }
            return true;
        }, IntPtr.Zero);
        return found;
    }
    public static RECT GetRect(IntPtr h) { RECT r; GetWindowRect(h, out r); return r; }
    public static RECT MakeRect(int left, int top, int w, int h) {
        RECT r; r.Left = left; r.Top = top; r.Right = left + w; r.Bottom = top + h; return r;
    }

    // 采样 9 点：上沿外 4px 的 5 点（贴顶时改下沿外 4px）+ 左右两侧外 4px 各 2 点；
    // 返回各点亮度（601 加权，失败 -1），median 取有效点的低中位数（与产品实现一致）
    public static int[] Sample(RECT r, ref int median, ref int count) {
        int sampleY = (r.Top - 4 >= 0) ? r.Top - 4 : r.Bottom + 4;
        int cx = (r.Left + r.Right) / 2;
        int cy = (r.Top + r.Bottom) / 2;
        int[][] pts = new int[9][];
        int[] dx = { -48, -24, 0, 24, 48 };
        for (int i = 0; i < 5; i++) pts[i] = new int[] { cx + dx[i], sampleY };
        pts[5] = new int[] { r.Left - 4, cy - 8 };
        pts[6] = new int[] { r.Left - 4, cy + 8 };
        pts[7] = new int[] { r.Right + 4, cy - 8 };
        pts[8] = new int[] { r.Right + 4, cy + 8 };

        int[] lum = new int[9];
        IntPtr dc = GetDC(IntPtr.Zero);
        for (int i = 0; i < 9; i++) {
            uint c = GetPixel(dc, pts[i][0], pts[i][1]);
            if (c == 0xFFFFFFFFu) { lum[i] = -1; continue; }
            int rr = (int)(c & 0xFF), gg = (int)((c >> 8) & 0xFF), bb = (int)((c >> 16) & 0xFF);
            lum[i] = (rr * 299 + gg * 587 + bb * 114) / 1000;
        }
        ReleaseDC(IntPtr.Zero, dc);

        int[] valid = Array.FindAll(lum, delegate(int v) { return v >= 0; });
        Array.Sort(valid);
        count = valid.Length;
        median = (count == 0) ? -1 : valid[(count - 1) / 2];
        return lum;
    }
}
"@

# 合成场景：bright=亮场 / dark=暗场 / subtitle=暗底白字压采样区 / letterbox=上下黑边
function New-SceneBitmap([string]$name, [int]$w, [int]$h) {
    $bmp = New-Object Drawing.Bitmap -ArgumentList $w, $h
    $g = [Drawing.Graphics]::FromImage($bmp)
    switch ($name) {
        "bright" { $g.Clear([Drawing.Color]::FromArgb(232, 232, 232)) }
        "dark" { $g.Clear([Drawing.Color]::FromArgb(20, 20, 20)) }
        "subtitle" {
            $g.Clear([Drawing.Color]::FromArgb(30, 30, 30))
            $font = New-Object Drawing.Font -ArgumentList "Microsoft YaHei", 44, ([Drawing.FontStyle]::Bold)
            $text = "字幕测试行 —— 白字压在悬浮窗采样区"
            $tw = $g.MeasureString($text, $font).Width
            foreach ($dy in 230, 160, 90) {
                $g.DrawString($text, $font, [Drawing.Brushes]::White, 60, ($h - $dy))
                $g.DrawString($text, $font, [Drawing.Brushes]::White, ($w - $tw - 40), ($h - $dy))
            }
            $font.Dispose()
        }
        "letterbox" {
            $g.Clear([Drawing.Color]::Black)
            $ch = [int]($h * 0.55)
            $g.FillRectangle([Drawing.Brushes]::Silver, 0, [int](($h - $ch) / 2), $w, $ch)
        }
    }
    $g.Dispose()
    return $bmp
}

# DPI：与产品一致按物理像素采样（DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2 = -4）
[void][BgSample]::SetThreadDpiAwarenessContext([IntPtr](-4))
$screenW = [BgSample]::GetSystemMetrics(0)
$screenH = [BgSample]::GetSystemMetrics(1)

$useWindow = $true
# -Rect 兼容 "左,上,宽,高" 与 "左 上 宽 高"（经 powershell -File 调用时逗号表达式会变成单个字符串）
$RectArr = @()
if ($Rect) { $RectArr = @($Rect -split '[,\s]+' | Where-Object { $_ -ne "" } | ForEach-Object { [int]$_ }) }
if ($RectArr.Count -ne 0 -and $RectArr.Count -ne 4) {
    throw "参数 -Rect 需要 4 个整数：左,上,宽,高（收到 $($RectArr.Count) 个）"
}
if ($RectArr.Count -eq 4) { $useWindow = $false }

# 合成场景：脚本自己当全屏"视频"，避免用终端运行脚本时抢前台导致悬浮窗隐藏
$form = $null
$sceneBmp = $null
$scenes = @()
if ($Scene -and $useWindow) {
    Add-Type -AssemblyName System.Windows.Forms
    Add-Type -AssemblyName System.Drawing
    if ($Scene -eq "cycle") { $scenes = @("bright", "dark", "subtitle", "letterbox") } else { $scenes = @($Scene) }
    $form = New-Object Windows.Forms.Form
    $form.FormBorderStyle = [Windows.Forms.FormBorderStyle]::None
    $form.StartPosition = [Windows.Forms.FormStartPosition]::Manual
    $form.Bounds = New-Object Drawing.Rectangle -ArgumentList 0, 0, $screenW, $screenH
    $form.Text = "TimeStick bg test"
    $form.TopMost = $true
    $form.ShowInTaskbar = $false
    $form.BackgroundImageLayout = [Windows.Forms.ImageLayout]::None
    $form.Show()
    $form.Activate()
}

if ($useWindow) {
    Write-Host "等待悬浮窗出现（进入全屏后约 2~3 秒）…"
    $waited = 0
    while ($true) {
        if ($form) { [Windows.Forms.Application]::DoEvents() }
        $hwnd = [BgSample]::FindFloater()
        if ($hwnd -ne [IntPtr]::Zero) { break }
        if ($waited -ge ($WaitSeconds * 1000)) {
            if ($form) { $form.Close() }
            throw "等待悬浮窗超时：未检测到全屏画面，或悬浮窗未显示。请先启动 timestick.exe；也可用 -Rect 左,上,宽,高 手工指定采样区域。"
        }
        Start-Sleep -Milliseconds 300
        $waited += 300
    }
    $r = [BgSample]::GetRect($hwnd)
    $source = "floater"
} else {
    $hwnd = [IntPtr]::Zero
    $r = [BgSample]::MakeRect($RectArr[0], $RectArr[1], $RectArr[2], $RectArr[3])
    $source = "rect"
}

if (-not $Out) {
    $dir = Join-Path (Split-Path $PSScriptRoot -Parent) "target"
    if (-not (Test-Path $dir)) { New-Item -ItemType Directory -Path $dir | Out-Null }
    $tag = if ($Scene) { $Scene } else { $Label }
    $Out = Join-Path $dir ("bg_lum_" + $tag + "_" + (Get-Date -Format "yyyyMMdd_HHmmss") + ".csv")
}
$utf8 = New-Object System.Text.UTF8Encoding($false)
$head = "# scene=$Scene label=$Label source=$source tick=$($TickMs)ms rect=$($r.Left),$($r.Top),$($r.Right),$($r.Bottom) screen=${screenW}x${screenH}"
[IO.File]::AppendAllText($Out, $head + "`r`n", $utf8)
[IO.File]::AppendAllText($Out, "time,label,n,median,mean,x1,x2,x3,x4,x5,x6,x7,x8,x9`r`n", $utf8)
Write-Host "开始采样：$Seconds 秒 / 每 $TickMs ms / 来源 $source / 场景 $(if ($Scene) { $Scene } else { '真实视频' })"

$collectedMs = 0
$absentMs = 0
$lastSec = -1
$sceneIndex = -1
$medians = New-Object System.Collections.Generic.List[int]
$empty = 0
try {
    while ($collectedMs -lt ($Seconds * 1000)) {
        # 悬浮窗不在（切出全屏）时暂停，不写空样本
        if ($useWindow) {
            $hwnd = [BgSample]::FindFloater()
            if ($hwnd -eq [IntPtr]::Zero) {
                $absentMs += 300
                if ($absentMs -gt ($WaitSeconds * 1000)) {
                    Write-Host "采样中断：悬浮窗长时间未出现"
                    break
                }
                Start-Sleep -Milliseconds 300
                continue
            }
            $absentMs = 0
            $r = [BgSample]::GetRect($hwnd)
        }

        # 合成场景按需切换画面（cycle 模式轮换四种场景）
        if ($form) {
            $idx = 0
            if ($Scene -eq "cycle") { $idx = [int][Math]::Floor($collectedMs / ($SceneSeconds * 1000)) % $scenes.Count }
            if ($idx -ne $sceneIndex) {
                $sceneIndex = $idx
                if ($sceneBmp) { $sceneBmp.Dispose() }
                $sceneBmp = New-SceneBitmap $scenes[$idx] $screenW $screenH
                # WinForms 画 BackgroundImage 前先用 BackColor 擦背景：底色取新场景左上角像素，
                # 否则默认浅灰(240)会在每次切场景时闪成"亮背景"，被反色采样当成一次真翻转
                $form.BackColor = $sceneBmp.GetPixel(0, 0)
                $form.BackgroundImage = $sceneBmp
                $form.Refresh()
                Write-Host ("场景切换 → {0}" -f $scenes[$idx])
            }
        }

        $median = 0
        $count = 0
        $lum = [BgSample]::Sample($r, [ref]$median, [ref]$count)
        $sum = 0
        foreach ($v in $lum) { if ($v -ge 0) { $sum += $v } }
        $mean = if ($count -gt 0) { [int][Math]::Round($sum / $count) } else { -1 }
        if ($count -gt 0) { [void]$medians.Add($median) } else { $empty++ }

        $rowLabel = $Label
        if ($Scene -eq "cycle") { $rowLabel = $scenes[$sceneIndex] } elseif ($Scene) { $rowLabel = $Scene }
        $cols = ($lum | ForEach-Object { if ($_ -ge 0) { $_ } else { "" } }) -join ","
        $row = "{0},{1},{2},{3},{4},{5}" -f (Get-Date -Format "HH:mm:ss.fff"), $rowLabel, $count, $median, $mean, $cols
        [IO.File]::AppendAllText($Out, $row + "`r`n", $utf8)

        $collectedMs += $TickMs
        $sec = [int][Math]::Floor($collectedMs / 1000)
        if ($sec -ne $lastSec) {
            $lastSec = $sec
            Write-Host ("[{0}/{1}s] {2} n={3} median={4} mean={5}" -f $sec, $Seconds, $rowLabel, $count, $median, $mean)
        }
        if ($form) { [Windows.Forms.Application]::DoEvents() }
        Start-Sleep -Milliseconds $TickMs
    }
} finally {
    if ($sceneBmp) { $sceneBmp.Dispose() }
    if ($form) { $form.Close(); $form.Dispose() }
}

$vals = @($medians | Sort-Object)
if ($vals.Count -eq 0) {
    Write-Host "本轮无有效样本（GetPixel 全部失败）——CSV: $Out"
    exit 1
}
function Pct($sorted, [double]$p) {
    $i = [int][Math]::Floor(($sorted.Count - 1) * $p)
    return $sorted[$i]
}
$summaryLabel = $Scene
if (-not $summaryLabel) { $summaryLabel = $Label }
Write-Host ""
Write-Host ("场景 {0}：有效样本 {1}，n=0 共 {2} 次，时长 {3}s" -f $summaryLabel, $vals.Count, $empty, $Seconds)
Write-Host ("中位数分布 min={0} P5={1} P25={2} P50={3} P75={4} P95={5} max={6}" -f `
        $vals[0], (Pct $vals 0.05), (Pct $vals 0.25), (Pct $vals 0.5), (Pct $vals 0.75), (Pct $vals 0.95), $vals[$vals.Count - 1])
Write-Host ("CSV: {0}" -f $Out)
