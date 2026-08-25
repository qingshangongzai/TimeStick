# TimeStick 位置持久化验证：模拟拖动悬浮窗 → 保存 → 重启 → 恢复到原位置
# 注意：本文件必须保持 UTF-8 带 BOM 编码（PowerShell 5.1 无 BOM 时按 ANSI 解析，中文会乱码）
$ErrorActionPreference = "Stop"
$exe = "D:\青山公仔\应用\Code\TimeStick\target\release\timestick.exe"
$log = "$env:TEMP\timestick_pos.log"
$cfg = "$env:APPDATA\TimeStick\position.txt"
$dragDX = -300   # 拖动位移（左上方向，保证初始右下角位置移动后仍在屏内）
$dragDY = -200

Add-Type @"
using System;
using System.Text;
using System.Runtime.InteropServices;
public class PosWin {
    static IntPtr hwnd;
    [DllImport("user32.dll")] public static extern bool EnumWindows(PosWin.EnumProc cb, IntPtr lParam);
    public delegate bool EnumProc(IntPtr hWnd, IntPtr lParam);
    [DllImport("user32.dll")] public static extern int GetClassName(IntPtr hWnd, StringBuilder sb, int max);
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
}
"@

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

# 0. 清理（避免单实例互斥与旧配置干扰测试）
Stop-Process -Name timestick -Force -ErrorAction SilentlyContinue
Start-Sleep -Milliseconds 500
Remove-Item $log -ErrorAction SilentlyContinue
Remove-Item $cfg -ErrorAction SilentlyContinue
Write-Host "0. 已清理旧实例/日志/配置"

# 1. 启动 → 全屏 → 悬浮窗默认位置出现
$env:TIMESTICK_LOG = $log
$p = Start-Process -FilePath $exe -PassThru
Start-Sleep -Seconds 1
$null = [PosWin]::CreateFullscreenWindow()
Start-Sleep -Seconds 5   # 轮询1s + 防抖1.5s + 余量
$fh = Get-FloaterWindow
if (-not $fh -or $fh -eq [IntPtr]::Zero) { Write-Host "!! 未找到悬浮窗（阶段1失败）"; exit 1 }
$r0 = New-Object PosWin+RECT
[PosWin]::GetWindowRect($fh, [ref]$r0) | Out-Null
Write-Host "1. 悬浮窗初始位置: ($($r0.Left),$($r0.Top)) 尺寸=$($r0.Right-$r0.Left)x$($r0.Bottom-$r0.Top)"

# 2. 模拟真实拖动：光标居中 → 按下 → 移动光标 → WM_MOUSEMOVE → 松开
#    （悬浮窗位置由 UpdateLayeredWindow 管理，外部 SetWindowPos 会被渲染覆盖，
#     必须走 on_mousemove 的拖动路径更新 self.pos）
$sw = [PosWin]::GetSystemMetrics(0); $sh = [PosWin]::GetSystemMetrics(1)
[PosWin]::SetCursorPos([int]($sw / 2), [int]($sh / 2)) | Out-Null
Start-Sleep -Milliseconds 100
$c0 = New-Object PosWin+POINT
[PosWin]::GetCursorPos([ref]$c0) | Out-Null
$cy = [math]::Floor(($r0.Bottom - $r0.Top) / 2)
$lp = 10 -bor ($cy -shl 16)   # 客户区左侧中部（避开右侧图钉/关闭按钮热区）
[PosWin]::PostMessage($fh, 0x0201, [IntPtr]1, $lp) | Out-Null   # WM_LBUTTONDOWN (MK_LBUTTON)
Start-Sleep -Milliseconds 100
[PosWin]::SetCursorPos($c0.X + $dragDX, $c0.Y + $dragDY) | Out-Null
Start-Sleep -Milliseconds 100
[PosWin]::PostMessage($fh, 0x0200, [IntPtr]1, $lp) | Out-Null   # WM_MOUSEMOVE
Start-Sleep -Milliseconds 100
[PosWin]::PostMessage($fh, 0x0202, [IntPtr]0, $lp) | Out-Null   # WM_LBUTTONUP
Start-Sleep -Milliseconds 500
$expectedX = $r0.Left + $dragDX
$expectedY = $r0.Top + $dragDY
if (-not (Test-Path $cfg)) { Write-Host "!! 配置文件未生成（保存失败）"; exit 1 }
$content = (Get-Content $cfg -Raw).Trim()
Write-Host "2. 已模拟拖动 ($dragDX,$dragDY)，期望位置 ($expectedX,$expectedY)，配置内容: '$content'"

# 3. 重启进程 → 悬浮窗应在保存的位置恢复
Stop-Process -Name timestick -Force -ErrorAction SilentlyContinue
Start-Sleep -Milliseconds 800
$p = Start-Process -FilePath $exe -PassThru
Start-Sleep -Seconds 5   # 轮询1s + 防抖1.5s + 余量
$fh2 = Get-FloaterWindow
if (-not $fh2 -or $fh2 -eq [IntPtr]::Zero) { Write-Host "!! 重启后未找到悬浮窗"; exit 1 }
$r2 = New-Object PosWin+RECT
[PosWin]::GetWindowRect($fh2, [ref]$r2) | Out-Null
$dx = [math]::Abs($r2.Left - $expectedX); $dy = [math]::Abs($r2.Top - $expectedY)
$moved = ($r2.Left -ne $r0.Left) -or ($r2.Top -ne $r0.Top)
Write-Host "3. 重启后悬浮窗位置: ($($r2.Left),$($r2.Top))  期望: ($expectedX,$expectedY)  偏差=($dx,$dy)px（容差 ±5）"
Write-Host "   位置已改变(非默认): $moved   恢复验证: $(if (($dx -le 5) -and ($dy -le 5) -and $moved) {'通过'} else {'失败'})"
$pass = ($dx -le 5) -and ($dy -le 5) -and $moved

# 4. 日志（期望含 position saved）
Write-Host "`n4. 日志关键行:"
Get-Content $log | Where-Object { $_ -match "position|floater shown" } | ForEach-Object { Write-Host "  $_" }

# 5. 清理（销毁全屏窗口、结束进程、删除测试配置）
[PosWin]::DestroyWindow2() | Out-Null
Stop-Process -Name timestick -Force -ErrorAction SilentlyContinue
Remove-Item $cfg -ErrorAction SilentlyContinue
Write-Host "`n5. 已清理，测试结束。结果: $(if ($pass) {'全部通过'} else {'存在失败项'})"
if (-not $pass) { exit 1 }
