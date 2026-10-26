# TimeStick 位置验证：记忆模式拖动/重启恢复 + 九宫格切换立即生效/跨重启保持/拖动切回记忆
# 前置：cargo build --release；须在可交互桌面执行（脚本会弹全屏窗口并多次重启 timestick.exe）
# 注意：本文件必须保持 UTF-8 带 BOM 编码（PowerShell 5.1 无 BOM 时按 ANSI 解析，中文会乱码）
$ErrorActionPreference = "Stop"

$root = Split-Path $PSScriptRoot -Parent
$exe = Join-Path $root "target\release\timestick.exe" # 从脚本位置推导，不硬编码绝对路径
$log = "$env:TEMP\timestick_pos.log"
$settings = "$env:APPDATA\TimeStick\settings.txt"
$settingsBak = "$settings.bak"
$cfg = "$env:APPDATA\TimeStick\position.txt"
$cfgBak = "$cfg.bak"
$dragDX = -300   # 拖动位移（左上方向，保证初始右下角位置移动后仍在屏内）
$dragDY = -200
$tol = 5         # 位置断言容差（px）
$fails = @()

if (-not (Test-Path $exe)) { Write-Host "FAIL: 未找到 $exe（先 cargo build --release）"; exit 1 }

Add-Type @"
using System;
using System.Text;
using System.Runtime.InteropServices;
public class PosWin {
    static IntPtr hwnd;
    [DllImport("user32.dll")] public static extern bool SetProcessDpiAwarenessContext(IntPtr c);
    [DllImport("user32.dll")] public static extern bool EnumWindows(PosWin.EnumProc cb, IntPtr lParam);
    public delegate bool EnumProc(IntPtr hWnd, IntPtr lParam);
    [DllImport("user32.dll")] public static extern int GetClassName(IntPtr hWnd, StringBuilder sb, int max);
    [DllImport("user32.dll", CharSet = CharSet.Unicode)] public static extern IntPtr FindWindowW(string cls, string name);
    [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr hWnd, out RECT r);
    [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr hWnd);
    [DllImport("user32.dll")] public static extern IntPtr GetForegroundWindow();
    [DllImport("user32.dll")] public static extern int GetSystemMetrics(int i);
    [DllImport("user32.dll", CharSet = CharSet.Unicode)] public static extern IntPtr CreateWindowEx(int ex, string cls, string name, uint style, int x, int y, int w, int h, IntPtr parent, IntPtr menu, IntPtr inst, IntPtr param);
    [DllImport("user32.dll")] public static extern bool ShowWindow(IntPtr h, int c);
    [DllImport("user32.dll")] public static extern bool DestroyWindow(IntPtr h);
    [DllImport("user32.dll")] public static extern void keybd_event(byte vk, byte scan, uint flags, UIntPtr extra);
    [DllImport("kernel32.dll")] public static extern IntPtr GetModuleHandle(string n);
    [DllImport("user32.dll")] public static extern bool PostMessage(IntPtr h, uint msg, IntPtr w, int l);
    [DllImport("user32.dll")] public static extern bool GetCursorPos(out POINT p);
    [DllImport("user32.dll")] public static extern bool SetCursorPos(int x, int y);
    public struct RECT { public int Left, Top, Right, Bottom; }
    public struct POINT { public int X, Y; }

    public static IntPtr CreateFullscreenWindow() {
        int w = GetSystemMetrics(0), h = GetSystemMetrics(1);
        // WS_POPUP(0x80000000) | WS_VISIBLE(0x10000000)，无 WS_MAXIMIZE
        hwnd = CreateWindowEx(0, "STATIC", "", 0x80000000u | 0x10000000u, 0, 0, w, h, IntPtr.Zero, IntPtr.Zero, GetModuleHandle(null), IntPtr.Zero);
        ShowWindow(hwnd, 5); // SW_SHOW
        // 模拟 ALT 键使本进程获得前台资格，再 SetForegroundWindow（解除前台锁限制）
        keybd_event(0x12, 0, 0, UIntPtr.Zero);
        keybd_event(0x12, 0, 2, UIntPtr.Zero);
        SetForegroundWindow(hwnd);
        return hwnd;
    }
    public static void DestroyWindow2() { if (hwnd != IntPtr.Zero) DestroyWindow(hwnd); }
    // PS 5.1 陷阱：脚本传 $null 会被编组成空串致 FindWindow 失败，null 参数必须写在 C# 侧
    public static IntPtr FindMain() { return FindWindowW("TimeStickMain", null); }
}
"@

# 与 timestick 同为 PER_MONITOR_AWARE_V2：坐标口径统一为物理像素。
# 不声明时脚本（DPI unaware）拿到的是虚拟化坐标，150% 缩放下 GetSystemMetrics/GetWindowRect 均 ÷1.5，
# 而格位边距 24px 是物理像素，格位断言会整体偏移（实测 1707x1067 vs 2560x1600）。
$null = [PosWin]::SetProcessDpiAwarenessContext([IntPtr]-4)  # -4 = DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2

$WM_COMMAND = 0x0111
$CMD_POS_CENTER = 1022  # tray::TRAY_CMD_POS_MC（居中，模式 5）
$CMD_POS_BL = 1024      # tray::TRAY_CMD_POS_BL（左下，模式 7）

# 结束应用：单实例互斥体需要时间释放，下一轮才能重新起进程
function Stop-App {
    Stop-Process -Name timestick -Force -ErrorAction SilentlyContinue
    Start-Sleep -Milliseconds 500
}

# 备份只做一次：多轮用例共用同一份原件，finally 才能还原到运行前内容
function Backup-Once([string]$file, [string]$bak) {
    if ((Test-Path $file) -and -not (Test-Path $bak)) { Copy-Item $file $bak -Force }
}

function Get-FloaterWindow {
    $script:floater = [IntPtr]::Zero
    [PosWin]::EnumWindows({ param($h, $l)
        $sb = New-Object System.Text.StringBuilder 256
        [PosWin]::GetClassName($h, $sb, 256) | Out-Null
        if ($sb.ToString() -eq "TimeStickFloater") { $script:floater = $h; return $false }
        return $true
    }, [IntPtr]::Zero) | Out-Null
    return $script:floater
}

function Get-Rect([IntPtr]$h) {
    $r = New-Object PosWin+RECT
    [PosWin]::GetWindowRect($h, [ref]$r) | Out-Null
    return $r
}

# 模拟真实拖动：光标居中 → 按下（客户区左侧中部，避开右侧按钮热区）→ 移动光标 → WM_MOUSEMOVE → 松开
# （悬浮窗位置由 UpdateLayeredWindow 管理，外部 SetWindowPos 会被渲染覆盖，必须走 on_mousemove 的拖动路径）
function Invoke-Drag([IntPtr]$fh, [int]$winH) {
    $sw = [PosWin]::GetSystemMetrics(0); $sh = [PosWin]::GetSystemMetrics(1)
    [PosWin]::SetCursorPos([int]($sw / 2), [int]($sh / 2)) | Out-Null
    Start-Sleep -Milliseconds 100
    $c0 = New-Object PosWin+POINT
    [PosWin]::GetCursorPos([ref]$c0) | Out-Null
    $cy = [math]::Floor($winH / 2)
    $lp = 10 -bor ($cy -shl 16)
    [PosWin]::PostMessage($fh, 0x0201, [IntPtr]1, $lp) | Out-Null   # WM_LBUTTONDOWN (MK_LBUTTON)
    Start-Sleep -Milliseconds 100
    [PosWin]::SetCursorPos($c0.X + $dragDX, $c0.Y + $dragDY) | Out-Null
    Start-Sleep -Milliseconds 100
    [PosWin]::PostMessage($fh, 0x0200, [IntPtr]1, $lp) | Out-Null   # WM_MOUSEMOVE
    Start-Sleep -Milliseconds 100
    [PosWin]::PostMessage($fh, 0x0202, [IntPtr]0, $lp) | Out-Null   # WM_LBUTTONUP
    Start-Sleep -Milliseconds 500
}

function Check([string]$name, [bool]$ok, [string]$detail) {
    if ($ok) { Write-Host "   PASS: $name（$detail）" }
    else { Write-Host "   FAIL: $name（$detail）"; $script:fails += "${name}: $detail" }
}

try {
    # ============ 0. 清理（避免单实例互斥与旧配置干扰测试）============
    Stop-App
    Remove-Item $log -ErrorAction SilentlyContinue
    New-Item -ItemType Directory -Force -Path (Split-Path $settings -Parent) | Out-Null
    Backup-Once $settings $settingsBak
    Backup-Once $cfg $cfgBak
    # 应用以全默认设置启动（position_mode=0），既有记忆模式三阶段不受旧设置污染
    Remove-Item $settings -ErrorAction SilentlyContinue
    Remove-Item $cfg -ErrorAction SilentlyContinue
    Write-Host "0. 已清理旧实例/日志，settings.txt 与 position.txt 重置为默认"

    $sw = [PosWin]::GetSystemMetrics(0); $sh = [PosWin]::GetSystemMetrics(1)

    # ============ 1. 启动 → 全屏 → 悬浮窗默认位置出现 ============
    $env:TIMESTICK_LOG = $log
    Start-Process -FilePath $exe | Out-Null
    Start-Sleep -Seconds 1
    $null = [PosWin]::CreateFullscreenWindow()
    Start-Sleep -Seconds 5   # 轮询1s + 防抖1.5s + 余量
    $fh = Get-FloaterWindow
    if ($fh -eq [IntPtr]::Zero) { throw "1: 未找到悬浮窗" }
    $r0 = Get-Rect $fh
    $w = $r0.Right - $r0.Left; $h = $r0.Bottom - $r0.Top
    Write-Host "1. 悬浮窗初始位置: ($($r0.Left),$($r0.Top)) 尺寸=${w}x${h}"

    # ============ 2. 记忆模式拖动 → 落盘 position.txt ============
    Invoke-Drag $fh $h
    $expectedX = $r0.Left + $dragDX
    $expectedY = $r0.Top + $dragDY
    if (-not (Test-Path $cfg)) { $fails += "2: position.txt 未生成（保存失败）" }
    $content = (Get-Content $cfg -Raw).Trim()
    Write-Host "2. 已模拟拖动 ($dragDX,$dragDY)，期望位置 ($expectedX,$expectedY)，配置内容: '$content'"

    # ============ 3. 重启进程 → 悬浮窗应在保存的位置恢复 ============
    Stop-App
    Start-Process -FilePath $exe | Out-Null
    Start-Sleep -Seconds 5   # 轮询1s + 防抖1.5s + 余量
    $fh2 = Get-FloaterWindow
    if ($fh2 -eq [IntPtr]::Zero) { throw "3: 重启后未找到悬浮窗" }
    $r2 = Get-Rect $fh2
    $dx = [math]::Abs($r2.Left - $expectedX); $dy = [math]::Abs($r2.Top - $expectedY)
    $moved = ($r2.Left -ne $r0.Left) -or ($r2.Top -ne $r0.Top)
    Write-Host "3. 重启后悬浮窗位置: ($($r2.Left),$($r2.Top))  期望: ($expectedX,$expectedY)  偏差=($dx,$dy)px（容差 ±$tol）"
    Check "记忆模式跨重启恢复" (($dx -le $tol) -and ($dy -le $tol) -and $moved) "偏差=($dx,$dy)px，位置已改变=$moved"

    # ============ 4a. 九宫格：切「居中」立即移窗 ============
    $main = [PosWin]::FindMain()
    if ($main -eq [IntPtr]::Zero) { throw "4a: 未找到主窗口（TimeStickMain）" }
    [PosWin]::PostMessage($main, $WM_COMMAND, [IntPtr]$CMD_POS_CENTER, 0) | Out-Null
    Start-Sleep -Milliseconds 500
    $fh3 = Get-FloaterWindow
    $r4 = Get-Rect $fh3
    # 居中格与 grid_point 公式严格相等：24px 对称内缩不改变屏中心
    $ex = [math]::Floor($sw / 2 - $w / 2); $ey = [math]::Floor($sh / 2 - $h / 2)
    $dx = [math]::Abs($r4.Left - $ex); $dy = [math]::Abs($r4.Top - $ey)
    Write-Host "4a. 切「居中」后位置: ($($r4.Left),$($r4.Top))  期望: ($ex,$ey)  偏差=($dx,$dy)px"
    Check "九宫格切换立即生效" (($dx -le $tol) -and ($dy -le $tol)) "居中格位偏差=($dx,$dy)px"
    $st = Get-Content $settings -Raw
    Check "九宫格模式持久化" ([bool]($st -match 'position_mode=5')) "settings.txt position_mode=5"

    # ============ 4b. 九宫格下拖动 → 自动切回记忆模式 ============
    Invoke-Drag $fh3 $h
    $expectBX = $r4.Left + $dragDX; $expectBY = $r4.Top + $dragDY
    $st = Get-Content $settings -Raw
    Check "拖动切回记忆模式" ([bool]($st -match 'position_mode=0')) "settings.txt position_mode=0"
    if (-not (Test-Path $cfg)) {
        $fails += "4b: 拖动后 position.txt 未生成"
    } else {
        $parts = ((Get-Content $cfg -Raw).Trim() -split '\s+')
        $dev = $parts[0]; $rx = [double]$parts[1]; $ry = [double]$parts[2]
        # 比例 4 位小数舍入误差 ≤0.1px，叠加移动取整，容差沿用 ±5px
        $px = [math]::Round($rx * $sw); $py = [math]::Round($ry * $sh)
        $dx = [math]::Abs($px - $expectBX); $dy = [math]::Abs($py - $expectBY)
        Write-Host "4b. 拖动后 $dev 比例=($rx,$ry) → 像素=($px,$py)  期望: ($expectBX,$expectBY)  偏差=($dx,$dy)px"
        Check "拖动后位置落盘" (($dx -le $tol) -and ($dy -le $tol)) "偏差=($dx,$dy)px"
    }

    # ============ 4c. 跨重启保持格位（左下 = 模式 7）============
    [PosWin]::PostMessage($main, $WM_COMMAND, [IntPtr]$CMD_POS_BL, 0) | Out-Null
    Start-Sleep -Milliseconds 500
    Stop-App
    Start-Process -FilePath $exe | Out-Null
    Start-Sleep -Seconds 5   # 轮询1s + 防抖1.5s + 余量
    $fh4 = Get-FloaterWindow
    if ($fh4 -eq [IntPtr]::Zero) { throw "4c: 重启后未找到悬浮窗" }
    $r5 = Get-Rect $fh4
    # 左下格位：整屏左边距 24px、下边距 24px（与 grid_point 公式一致）
    $ex = 24; $ey = $sh - 24 - $h
    $dx = [math]::Abs($r5.Left - $ex); $dy = [math]::Abs($r5.Top - $ey)
    Write-Host "4c. 跨重启左下落位: ($($r5.Left),$($r5.Top))  期望: ($ex,$ey)  偏差=($dx,$dy)px"
    Check "九宫格跨重启保持" (($dx -le $tol) -and ($dy -le $tol)) "偏差=($dx,$dy)px（绑定屏设备名→矩形解析链路）"

    # ============ 5. 日志（期望含 position mode / position saved）============
    Write-Host "`n5. 日志关键行:"
    Get-Content $log -ErrorAction SilentlyContinue | Where-Object { $_ -match "position|floater shown" } | ForEach-Object { Write-Host "  $_" }
} catch {
    $fails += "中断：$_"
} finally {
    # 收尾：销毁全屏窗口 → 结束进程 → 还原 settings.txt / position.txt（原本不存在则删掉测试期新生成的文件）
    [PosWin]::DestroyWindow2() | Out-Null
    Stop-App
    foreach ($pair in @(@($settings, $settingsBak), @($cfg, $cfgBak))) {
        $file, $bak = $pair
        if (Test-Path $bak) { Move-Item $bak $file -Force }
        elseif (Test-Path $file) { Remove-Item $file -Force }
    }
    Write-Host "`n已清理全屏窗口/进程，settings.txt 与 position.txt 已还原"
}

if ($fails.Count -eq 0) {
    Write-Host "`nPASS"
    exit 0
}
$fails | ForEach-Object { Write-Host "FAIL: $_" }
exit 1
