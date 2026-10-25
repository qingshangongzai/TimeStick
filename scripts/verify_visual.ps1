# TimeStick 视觉验证：全屏触发悬浮窗后截屏保存
$ErrorActionPreference = "Stop"
Add-Type -AssemblyName System.Windows.Forms
Add-Type -AssemblyName System.Drawing
$log = "$env:TEMP\timestick_shot.log"
$shot = "$env:TEMP\timestick_shot.png"
$exe = Join-Path $PSScriptRoot "..\target\release\timestick.exe"
Remove-Item $log, $shot -ErrorAction SilentlyContinue

Add-Type @"
using System;
using System.Runtime.InteropServices;
public class Shot2 {
    [DllImport("user32.dll")] public static extern bool EnumWindows(Shot2.EnumProc cb, IntPtr l);
    public delegate bool EnumProc(IntPtr h, IntPtr l);
    [DllImport("user32.dll")] public static extern int GetClassName(IntPtr h, System.Text.StringBuilder sb, int max);
    [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
    [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
    public struct RECT { public int Left, Top, Right, Bottom; }
    public static IntPtr FindFloater() {
        IntPtr found = IntPtr.Zero;
        EnumWindows(delegate(IntPtr h, IntPtr l) {
            var sb = new System.Text.StringBuilder(256);
            GetClassName(h, sb, 256);
            if (sb.ToString() == "TimeStickFloater" && IsWindowVisible(h)) { found = h; return false; }
            return true;
        }, IntPtr.Zero);
        return found;
    }
    public static RECT GetRect(IntPtr h) { RECT r; GetWindowRect(h, out r); return r; }
    [DllImport("user32.dll")] public static extern IntPtr CreateWindowEx(int ex, string cls, string name, uint style, int x, int y, int w, int h, IntPtr p, IntPtr m, IntPtr i, IntPtr l);
    [DllImport("user32.dll")] public static extern bool ShowWindow(IntPtr h, int c);
    [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h);
    [DllImport("user32.dll")] public static extern void keybd_event(byte vk, byte scan, uint flags, UIntPtr extra);
    [DllImport("user32.dll")] public static extern int GetSystemMetrics(int i);
    [DllImport("kernel32.dll")] public static extern IntPtr GetModuleHandle(string n);
    [DllImport("user32.dll")] public static extern bool DestroyWindow(IntPtr h);
    static IntPtr hwnd;
    public static IntPtr ShowFs() {
        int w = GetSystemMetrics(0), h = GetSystemMetrics(1);
        hwnd = CreateWindowEx(0, "STATIC", "", 0x80000000u | 0x10000000u, 0, 0, w, h, IntPtr.Zero, IntPtr.Zero, GetModuleHandle(null), IntPtr.Zero);
        ShowWindow(hwnd, 5);
        keybd_event(0x12, 0, 0, UIntPtr.Zero); keybd_event(0x12, 0, 2, UIntPtr.Zero);
        SetForegroundWindow(hwnd);
        return hwnd;
    }
    public static void HideFs() { if (hwnd != IntPtr.Zero) DestroyWindow(hwnd); }
}
"@

Stop-Process -Name timestick -Force -ErrorAction SilentlyContinue
Start-Sleep -Milliseconds 500
$env:TIMESTICK_LOG = $log
$p = Start-Process -FilePath $exe -PassThru
Start-Sleep -Seconds 1

[Shot2]::ShowFs() | Out-Null
Start-Sleep -Seconds 5
$fh = [Shot2]::FindFloater()
if ($fh) {
    $r = [Shot2]::GetRect($fh)
    $w = $r.Right - $r.Left; $h = $r.Bottom - $r.Top
    $bmp = New-Object System.Drawing.Bitmap($w, $h)
    $g = [System.Drawing.Graphics]::FromImage($bmp)
    $g.CopyFromScreen($r.Left, $r.Top, 0, 0, $bmp.Size)  # 截取屏幕上的悬浮窗区域
    $g.Dispose()
    $bmp.Save($shot, [System.Drawing.Imaging.ImageFormat]::Png)
    $bmp.Dispose()
    Write-Host "悬浮窗句柄=$fh 尺寸=${w}x${h} 已截屏: $shot"
} else {
    Write-Host "未找到悬浮窗"
    Get-Content $log -ErrorAction SilentlyContinue
}
[Shot2]::HideFs() | Out-Null
Start-Sleep -Seconds 2
Stop-Process -Name timestick -Force -ErrorAction SilentlyContinue
