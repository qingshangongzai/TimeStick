# TimeStick 全屏检测验证：纯 Win32 创建无 WS_MAXIMIZE 的铺满屏幕窗口触发悬浮窗
$ErrorActionPreference = "Stop"
$log = "$env:TEMP\timestick_fs.log"
Remove-Item $log -ErrorAction SilentlyContinue

Add-Type @"
using System;
using System.Text;
using System.Runtime.InteropServices;
public class FsWin {
    static IntPtr hwnd;
    [DllImport("user32.dll")] public static extern bool EnumWindows(FsWin.EnumProc cb, IntPtr lParam);
    public delegate bool EnumProc(IntPtr hWnd, IntPtr lParam);
    [DllImport("user32.dll")] public static extern int GetClassName(IntPtr hWnd, StringBuilder sb, int max);
    [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr hWnd);
    [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr hWnd, out RECT r);
    [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr hWnd);
    [DllImport("user32.dll")] public static extern IntPtr GetForegroundWindow();
    [DllImport("user32.dll")] public static extern int GetSystemMetrics(int i);
    [DllImport("user32.dll", CharSet = CharSet.Unicode)] public static extern IntPtr CreateWindowEx(int ex, string cls, string name, uint style, int x, int y, int w, int h, IntPtr parent, IntPtr menu, IntPtr inst, IntPtr param);
    [DllImport("user32.dll")] public static extern bool ShowWindow(IntPtr h, int c);
    [DllImport("user32.dll")] public static extern bool DestroyWindow(IntPtr h);
    [DllImport("user32.dll")] public static extern void keybd_event(byte vk, byte scan, uint flags, UIntPtr extra);
    [DllImport("kernel32.dll")] public static extern IntPtr GetModuleHandle(string n);
    public struct RECT { public int Left, Top, Right, Bottom; }

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

function Get-FloaterWindow {
    $script:floater = $null
    [FsWin]::EnumWindows({ param($h, $l)
        $sb = New-Object System.Text.StringBuilder 256
        [FsWin]::GetClassName($h, $sb, 256) | Out-Null
        if ($sb.ToString() -eq "TimeStickFloater") { $script:floater = $h; return $false }
        return $true
    }, [IntPtr]::Zero) | Out-Null
    return $script:floater
}

# 0. 清理残留实例（避免单实例互斥锁干扰测试）
Stop-Process -Name timestick -Force -ErrorAction SilentlyContinue
Start-Sleep -Milliseconds 500

# 1. 启动
$env:TIMESTICK_LOG = $log
$p = Start-Process -FilePath "D:\青山公仔\应用\Code\TimeStick\target\release\timestick.exe" -PassThru
Start-Sleep -Seconds 1
Write-Host "TimeStick PID=$($p.Id)"

# 2. 创建全屏窗口（无 WS_MAXIMIZE）
$hwnd = [FsWin]::CreateFullscreenWindow()
Write-Host "全屏测试窗口: $hwnd  前台窗口: $([FsWin]::GetForegroundWindow())"
Start-Sleep -Seconds 5  # 轮询1s + 防抖1.5s + 余量

Write-Host "`n=== 日志（期望 fullscreen entered + floater shown）==="
Get-Content $log | ForEach-Object { Write-Host "  $_" }

$fh = Get-FloaterWindow
if ($fh) {
    $r = New-Object FsWin+RECT
    [FsWin]::GetWindowRect($fh, [ref]$r) | Out-Null
    $sw = [FsWin]::GetSystemMetrics(0); $sh = [FsWin]::GetSystemMetrics(1)
    Write-Host "`n=== 悬浮窗状态 ==="
    Write-Host "  窗口句柄: $fh  可见: $([FsWin]::IsWindowVisible($fh))"
    Write-Host "  矩形: ($($r.Left),$($r.Top))-($($r.Right),$($r.Bottom))  尺寸=$($r.Right-$r.Left)x$($r.Bottom-$r.Top)"
    Write-Host "  屏幕: ${sw}x${sh}"
    $nearBR = ($r.Right -ge $sw - 100) -and ($r.Bottom -ge $sh - 100)
    Write-Host "  位于右下角: $nearBR"
} else {
    Write-Host "`n!! 未找到悬浮窗"
}

# 3. 关闭全屏窗口 → 立即隐藏（隐藏即销毁：窗口与渲染器随隐藏释放，见内存优化方案）
Write-Host "`n=== 关闭全屏窗口 ==="
[FsWin]::DestroyWindow2() | Out-Null
Start-Sleep -Seconds 3
$fh2 = Get-FloaterWindow
if ($fh2 -eq $null) {
    Write-Host "悬浮窗已销毁（隐藏即销毁，等价于不可见）"
} else {
    Write-Host "悬浮窗可见: $([FsWin]::IsWindowVisible($fh2)) （期望 False）"
}
Write-Host "日志（期望 fullscreen exited + floater hidden）:"
Get-Content $log | Select-Object -Last 3 | ForEach-Object { Write-Host "  $_" }

# 4. 清理
Stop-Process -Name timestick -Force -ErrorAction SilentlyContinue
Write-Host "`n测试结束"
