# TimeStick 内存回落验证：显示即渲染、隐藏即销毁（内存回落至静默基线）；
# 重建路径完整恢复（窗口位置/可见性 + 固定模式鼠标穿透样式）。任一断言失败退出码非 0。
$ErrorActionPreference = "Stop"
$log = "$env:TEMP\timestick_mem.log"
$exePath = Join-Path $PSScriptRoot "..\target\release\timestick.exe"
# 前置检查放在任何副作用（杀进程 / 挪光标）之前：缺 exe 时不该先动用户的运行实例
if (-not (Test-Path $exePath)) { throw "未找到 $exePath，请先执行 cargo build --release" }
Remove-Item $log -ErrorAction SilentlyContinue

Add-Type @"
using System;
using System.Text;
using System.Runtime.InteropServices;
public class MemWin {
    static IntPtr hwnd;
    [DllImport("user32.dll")] public static extern bool EnumWindows(MemWin.EnumProc cb, IntPtr lParam);
    public delegate bool EnumProc(IntPtr hWnd, IntPtr lParam);
    [DllImport("user32.dll")] public static extern int GetClassName(IntPtr hWnd, StringBuilder sb, int max);
    [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr hWnd);
    [DllImport("user32.dll")] public static extern bool GetClientRect(IntPtr hWnd, out RECT r);
    [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr hWnd, out RECT r);
    [DllImport("user32.dll")] public static extern bool GetCursorPos(out POINT p);
    [DllImport("user32.dll")] public static extern bool SetCursorPos(int x, int y);
    [DllImport("user32.dll")] public static extern IntPtr SendMessageW(IntPtr hWnd, uint msg, IntPtr wParam, IntPtr lParam);
    [DllImport("user32.dll", EntryPoint = "GetWindowLongPtrW")] public static extern IntPtr GetWindowLongPtr(IntPtr hWnd, int nIndex);
    [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr hWnd);
    [DllImport("user32.dll")] public static extern int GetSystemMetrics(int i);
    [DllImport("user32.dll", CharSet = CharSet.Unicode)] public static extern IntPtr CreateWindowEx(int ex, string cls, string name, uint style, int x, int y, int w, int h, IntPtr parent, IntPtr menu, IntPtr inst, IntPtr param);
    [DllImport("user32.dll")] public static extern bool ShowWindow(IntPtr h, int c);
    [DllImport("user32.dll")] public static extern bool DestroyWindow(IntPtr h);
    [DllImport("user32.dll")] public static extern void keybd_event(byte vk, byte scan, uint flags, UIntPtr extra);
    [DllImport("kernel32.dll")] public static extern IntPtr GetModuleHandle(string n);
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
}
"@

Add-Type -TypeDefinition @"
using System.Runtime.InteropServices;
public static class MemDpi {
    [DllImport("shcore.dll")] public static extern int SetProcessDpiAwareness(int value);
}
"@
# PER_MONITOR_DPI_AWARE：GetClientRect 等返回物理像素（与 TimeStick 一致）。
# 否则 PowerShell 进程不感知 DPI，150% 缩放下坐标被虚拟化，图钉点击会偏移（实测失效）。
[MemDpi]::SetProcessDpiAwareness(2) | Out-Null

function Get-FloaterWindow {
    $script:floater = $null
    [MemWin]::EnumWindows({ param($h, $l)
        $sb = New-Object System.Text.StringBuilder 256
        [MemWin]::GetClassName($h, $sb, 256) | Out-Null
        if ($sb.ToString() -eq "TimeStickFloater") { $script:floater = $h; return $false }
        return $true
    }, [IntPtr]::Zero) | Out-Null
    return $script:floater
}

function Get-PrivateMemMB([int]$procId) {
    $p = Get-Process -Id $procId -ErrorAction SilentlyContinue
    if (-not $p) { return 0 }
    return [math]::Round($p.PrivateMemorySize64 / 1MB, 2)
}

# 图钉热区中心 X：按 render.rs 布局公式换算——宽度 = pad + text + gap + btn + gap + btn + pad，
# 故 x_pin = 客户区宽 - (pad + gap + 2*btn)，中心再加 btn/2。
# 不得写死偏移：按钮增减或切换档位都会使偏移失准，且会静默命中相邻热区。
# 档位表与间距常量需与 render.rs 的 FONT_LEVELS / PAD_X / BTN_GAP / BTN_SIZE 同批更新。
function Get-PinCenterX([int]$clientWidth) {
    $fonts = @(14.0, 18.0, 22.0, 28.0, 36.0)
    $level = 1  # 默认小档 18.0，与 render.rs 的 SIZE_LEVEL_DEFAULT 一致
    $cfg = Join-Path $env:APPDATA "TimeStick\settings.txt"
    if (Test-Path $cfg) {
        $m = Select-String -Path $cfg -Pattern '^size_level=(\d)'
        if ($m) { $level = [int]$m.Matches[0].Groups[1].Value }
    }
    # 文件可手改，非法档位回退默认档（与 settings.rs 的容错取向一致）
    if ($level -lt 0 -or $level -ge $fonts.Count) { $level = 1 }
    $scale = $fonts[$level] / 18.0
    $btn = [Math]::Max(14.0 * $scale, 14.0)  # 按钮只放大不缩小
    $gap = 3.0 * $scale
    $pad = 6.0 * $scale
    return [int][Math]::Round($clientWidth - ($pad + $gap + 2 * $btn) + $btn / 2)
}

$script:failed = $false
function Assert([bool]$cond, [string]$msg) {
    if ($cond) { Write-Host "  [PASS] $msg" }
    else { Write-Host "  [FAIL] $msg"; $script:failed = $true }
}

# 0. 清理残留实例（避免单实例互斥锁干扰测试）
Stop-Process -Name timestick -Force -ErrorAction SilentlyContinue
Start-Sleep -Milliseconds 500
# 光标移出悬浮窗可能出现的角落位置（记录原位置，结束时恢复）：
# 固定模式悬停 1.5s 自动解除，若悬浮窗恰好出现在光标下方会被提前解除，干扰"重建后样式恢复"断言
$origCursor = New-Object MemWin+POINT
[MemWin]::GetCursorPos([ref]$origCursor) | Out-Null
$cx = [int](([MemWin]::GetSystemMetrics(0)) / 2)
$cy = [int](([MemWin]::GetSystemMetrics(1)) / 2)
[MemWin]::SetCursorPos($cx, $cy) | Out-Null

# 1. 启动（静默驻留基线）
$env:TIMESTICK_LOG = $log
$p = Start-Process -FilePath $exePath -PassThru
Start-Sleep -Seconds 4
$memSilent = Get-PrivateMemMB $p.Id
Write-Host "`n[1] 静默驻留（从未显示）: 私有内存 ${memSilent}MB"
Assert ($memSilent -lt 3) "静默基线私有内存 < 3MB（实测 ${memSilent}MB）"

# 2. 首次全屏显示：渲染器创建，内存上升
[MemWin]::CreateFullscreenWindow() | Out-Null
Start-Sleep -Seconds 6  # 轮询1s + 防抖1.5s + 创建 + 余量
$fh1 = Get-FloaterWindow
$memShown = Get-PrivateMemMB $p.Id
Write-Host "`n[2] 首次全屏显示: 私有内存 ${memShown}MB"
Assert ($fh1 -ne $null) "悬浮窗已创建（TimeStickFloater 类窗口存在）"
Assert ([MemWin]::IsWindowVisible($fh1)) "悬浮窗可见"
Assert ($memShown -gt 5) "渲染器创建后私有内存 > 5MB（实测 ${memShown}MB）"

# 3. 固定模式：向图钉按钮发 WM_LBUTTONDOWN（lparam 取图钉热区中心，坐标随显示档位换算）
$r = New-Object MemWin+RECT
[MemWin]::GetClientRect($fh1, [ref]$r) | Out-Null
$x = Get-PinCenterX $r.Right
$y = [int]($r.Bottom / 2)
$lp = [IntPtr]([int64](($y -shl 16) -bor ($x -band 0xffff)))
[MemWin]::SendMessageW($fh1, 0x0201, [IntPtr]::Zero, $lp) | Out-Null  # WM_LBUTTONDOWN
Start-Sleep -Milliseconds 500
$pinnedLog = Get-Content $log | Select-String -SimpleMatch "pinned (mouse passthrough on)"
Write-Host "`n[3] 固定模式: 点击图钉 ($x, $y)"
Assert ($pinnedLog -ne $null) "日志出现 pinned (mouse passthrough on)"

# 4. 退出全屏：隐藏即销毁，内存回落
# 回落依赖 dwrite 进程级字体缓存空闲回收（实测 ~7s），固定等待易受负载波动影响，
# 改为轮询：最长 15s、每 1s 采样，低于阈值即通过；回落高原实测 ~3.1MB（含分配器堆保留）
[MemWin]::DestroyWindow2() | Out-Null
$memHidden = 0
for ($i = 0; $i -lt 15; $i++) {
    Start-Sleep -Seconds 1
    $memHidden = Get-PrivateMemMB $p.Id
    if ($memHidden -lt 3.5) { break }
}
$fh2 = Get-FloaterWindow
Write-Host "`n[4] 退出全屏（隐藏即销毁）: 私有内存 ${memHidden}MB"
Assert ($fh2 -eq $null) "悬浮窗窗口已销毁（EnumWindows 找不到 TimeStickFloater）"
Assert ($memHidden -lt 3.5) "隐藏后私有内存回落 < 3.5MB（实测 ${memHidden}MB）"

# 5. 再次全屏：重建路径 + 固定模式样式恢复
[MemWin]::CreateFullscreenWindow() | Out-Null
Start-Sleep -Seconds 6
$fh3 = Get-FloaterWindow
$ex = [MemWin]::GetWindowLongPtr($fh3, -20).ToInt64()  # GWL_EXSTYLE
$hasTransparent = ($ex -band 0x20) -ne 0  # WS_EX_TRANSPARENT
$exHex = "{0:X}" -f $ex
Write-Host "`n[5] 再次全屏（重建）: GWL_EXSTYLE=0x$exHex"
Assert ($fh3 -ne $null) "重建后悬浮窗重新出现"
Assert ([MemWin]::IsWindowVisible($fh3)) "重建后悬浮窗可见"
Assert ($hasTransparent) "重建后扩展样式含 WS_EX_TRANSPARENT（固定模式跨销毁保留）"

# 5b. 悬停 1.5s 自动解除固定（验收标准：重建后自动解除仍生效，日志验证）
# 光标移至悬浮窗中心，固定模式 250ms 轮询累计悬停时长，1.5s 触发自动解除
$wr = New-Object MemWin+RECT
[MemWin]::GetWindowRect($fh3, [ref]$wr) | Out-Null
$hx = [int](($wr.Left + $wr.Right) / 2)
$hy = [int](($wr.Top + $wr.Bottom) / 2)
[MemWin]::SetCursorPos($hx, $hy) | Out-Null
Start-Sleep -Seconds 3
$unpinLog = Get-Content $log | Select-String -SimpleMatch "hover timeout: unpinning"
Write-Host "`n[5b] 悬停自动解除: 光标移至 ($hx, $hy)"
Assert ($unpinLog -ne $null) "悬停 1.5s 自动解除固定（日志出现 hover timeout: unpinning）"

# 6. 清理与汇总
Write-Host "`n=== 实测内存轨迹 ==="
Write-Host "  静默驻留 ${memSilent}MB → 全屏显示 ${memShown}MB → 隐藏后 ${memHidden}MB"
Stop-Process -Name timestick -Force -ErrorAction SilentlyContinue
[MemWin]::SetCursorPos($origCursor.X, $origCursor.Y) | Out-Null
if ($script:failed) {
    Write-Host "`n结果: 失败（存在 FAIL 断言）"
    exit 1
}
Write-Host "`n结果: 全部通过"
exit 0
