# TimeStick 应用黑名单验证：命中黑名单的全屏不显示悬浮窗，移除条目后恢复显示
# 注意：本文件必须保持 UTF-8 带 BOM 编码（PowerShell 5.1 无 BOM 时按 ANSI 解析，中文会乱码）
$ErrorActionPreference = "Stop"

$exePath = Join-Path $PSScriptRoot "..\target\release\timestick.exe"
$log = Join-Path $env:TEMP "timestick_blacklist.log"
$cfg = Join-Path $env:APPDATA "TimeStick\blacklist.txt"
$utf8NoBom = New-Object System.Text.UTF8Encoding($false)

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
    [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr hWnd);
    [DllImport("user32.dll")] public static extern int GetSystemMetrics(int i);
    [DllImport("user32.dll", CharSet = CharSet.Unicode)] public static extern IntPtr CreateWindowEx(int ex, string cls, string name, uint style, int x, int y, int w, int h, IntPtr parent, IntPtr menu, IntPtr inst, IntPtr param);
    [DllImport("user32.dll")] public static extern bool ShowWindow(IntPtr h, int c);
    [DllImport("user32.dll")] public static extern bool DestroyWindow(IntPtr h);
    [DllImport("user32.dll")] public static extern void keybd_event(byte vk, byte scan, uint flags, UIntPtr extra);
    [DllImport("kernel32.dll")] public static extern IntPtr GetModuleHandle(string n);

    public static IntPtr CreateFullscreenWindow() {
        int w = GetSystemMetrics(0), h = GetSystemMetrics(1);
        // WS_POPUP(0x80000000) | WS_VISIBLE(0x10000000)，无 WS_MAXIMIZE 与边框
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

function Get-LogText {
    if (Test-Path $log) { return (Get-Content $log -Raw) } else { return "" }
}

$script:failed = 0
function Check($cond, $msg) {
    if ($cond) {
        Write-Host "  [PASS] $msg"
    } else {
        Write-Host "  [FAIL] $msg"
        $script:failed++
    }
}

# 0. 前置检查：exe 已构建
if (-not (Test-Path $exePath)) { throw "未找到 $exePath，请先执行 cargo build --release" }

# 1. 清理残留实例：单实例互斥锁检查先于日志初始化，残留实例存活时新进程静默退出、日志恒为空
Stop-Process -Name timestick -Force -ErrorAction SilentlyContinue
Start-Sleep -Milliseconds 500
if (@(Get-Process -Name timestick -ErrorAction SilentlyContinue).Count -gt 0) {
    throw "请先退出正在运行的 TimeStick（以管理员身份启动的安装版，本脚本无权结束）"
}

# 2. 备份真实配置（按字节还原，保证执行前后完全一致）
$hadCfg = Test-Path $cfg
$backup = if ($hadCfg) { [IO.File]::ReadAllBytes($cfg) } else { $null }

# 3. 写入脚本宿主的 exe 名：全屏窗口由本进程创建，前台进程即脚本宿主。
#    用 $PID 取而非硬编码 powershell.exe——pwsh 7 下进程名是 pwsh.exe，硬编码会静默不命中。
#    必须显式 UTF-8 无 BOM：Out-File / Set-Content 在 PS 5.1 默认 UTF-16LE，Rust 侧按 UTF-8 读取会失败
$hostExe = (Get-Process -Id $PID).Name + ".exe"
$cfgDir = Split-Path $cfg
if (-not (Test-Path $cfgDir)) { New-Item -ItemType Directory -Path $cfgDir -Force | Out-Null }
[IO.File]::WriteAllText($cfg, $hostExe + [Environment]::NewLine, $utf8NoBom)
Write-Host "黑名单条目: $hostExe"

try {
    # 4. 启动（TIMESTICK_LOG 须在 Start-Process 之前设置，供子进程继承）
    Remove-Item $log -ErrorAction SilentlyContinue
    $env:TIMESTICK_LOG = $log
    $p = Start-Process -FilePath $exePath -PassThru
    Start-Sleep -Seconds 1
    if ($p.HasExited) { throw "TimeStick 启动后立即退出（残留实例未清干净？）" }
    Write-Host "TimeStick PID=$($p.Id)"

    # 5. 正向：创建铺满屏幕的全屏窗口，等轮询 + 余量（Blocked 转换无防抖）
    Write-Host "`n=== 正向：命中黑名单的全屏不显示 ==="
    $null = [FsWin]::CreateFullscreenWindow()
    Start-Sleep -Seconds 6
    $text = Get-LogText
    Check ($text -match ("blacklist hit: .*" + [regex]::Escape($hostExe))) "日志含 blacklist hit: ...$hostExe"
    Check (-not ($text -match "fullscreen entered")) "日志不含 fullscreen entered"
    $fh = Get-FloaterWindow
    Check ($null -eq $fh) "未创建 TimeStickFloater 窗口"

    # 6. 退出全屏，等 detect 线程的 prev 回到 Normal；删除条目
    [FsWin]::DestroyWindow2() | Out-Null
    Start-Sleep -Seconds 2
    Remove-Item $cfg -ErrorAction SilentlyContinue

    # 7. 反向：同一个全屏窗口应恢复显示（轮询 1s + 防抖 1.5s + 余量）
    Write-Host "`n=== 反向：移除条目后恢复显示 ==="
    $null = [FsWin]::CreateFullscreenWindow()
    Start-Sleep -Seconds 6
    $text2 = Get-LogText
    Check ($text2 -match "fullscreen entered") "日志新增 fullscreen entered"
    $fh2 = Get-FloaterWindow
    Check ($null -ne $fh2) "找到 TimeStickFloater 窗口"
    if ($null -ne $fh2) { Check ([FsWin]::IsWindowVisible($fh2)) "TimeStickFloater 可见" }
} finally {
    [FsWin]::DestroyWindow2() | Out-Null
    Stop-Process -Name timestick -Force -ErrorAction SilentlyContinue
    if ($hadCfg) {
        [IO.File]::WriteAllBytes($cfg, $backup)
    } else {
        Remove-Item $cfg -ErrorAction SilentlyContinue
    }
    Write-Host "`n=== 日志末 10 行 ==="
    if (Test-Path $log) { Get-Content $log | Select-Object -Last 10 | ForEach-Object { Write-Host "  $_" } }
    Write-Host "已还原 blacklist.txt: $(if ($hadCfg) { '原内容' } else { '删除本次创建的文件' })"
}

Write-Host "`n结果: $(if ($script:failed -eq 0) { '全部 PASS' } else { "$script:failed 项 FAIL" })"
if ($script:failed -gt 0) { throw "黑名单验证失败：$script:failed 项断言未通过" }
