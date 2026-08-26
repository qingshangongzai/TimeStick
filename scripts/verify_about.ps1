# TimeStick 关于对话框验证脚本（阶段三：打开/防重入/许可文本/关闭/重开；阶段四：关闭按钮存在 + 无边框客户区=整窗）
$ErrorActionPreference = "Continue"
$exe = "D:\青山公仔\应用\Code\TimeStick\target\release\timestick.exe"
$log = "$env:TEMP\timestick_about.log"
Remove-Item $log -ErrorAction SilentlyContinue

# PS 5.1 陷阱（阶段二实证）：$null 编组成空串致 FindWindow 失败 → null 参数 C# 侧封装传入
Add-Type @"
using System;
using System.Text;
using System.Runtime.InteropServices;
public class AboutWin {
    [DllImport("user32.dll", CharSet = CharSet.Unicode)] static extern IntPtr FindWindowW(string cls, string title);
    [DllImport("user32.dll", CharSet = CharSet.Unicode)] static extern IntPtr FindWindowExW(IntPtr parent, IntPtr after, string cls, string title);
    [DllImport("user32.dll")] public static extern bool PostMessageW(IntPtr hWnd, uint msg, UIntPtr wParam, IntPtr lParam);
    [DllImport("user32.dll")] public static extern IntPtr SendMessageW(IntPtr hWnd, uint msg, IntPtr wParam, IntPtr lParam);
    [DllImport("user32.dll")] public static extern bool IsWindow(IntPtr hWnd);
    [DllImport("user32.dll")] public static extern bool EnumWindows(EnumProc cb, IntPtr lParam);
    public delegate bool EnumProc(IntPtr hWnd, IntPtr lParam);
    [DllImport("user32.dll", CharSet = CharSet.Unicode)] public static extern int GetClassName(IntPtr hWnd, StringBuilder sb, int max);
    [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr hWnd, out RECT r);
    [DllImport("user32.dll")] public static extern bool GetClientRect(IntPtr hWnd, out RECT r);
    public struct RECT { public int Left, Top, Right, Bottom; }

    public static IntPtr FindMain() { return FindWindowW("TimeStickMain", null); }
    public static IntPtr FindAbout() { return FindWindowW("TimeStickAbout", null); }
    public static IntPtr NextEdit(IntPtr parent, IntPtr after) { return FindWindowExW(parent, after, "Edit", null); }
    public static IntPtr FindChild(IntPtr parent, string cls) { return FindWindowExW(parent, IntPtr.Zero, cls, null); }
}
"@

$WM_COMMAND = 0x0111
$WM_CLOSE = 0x0010
$WM_GETTEXTLENGTH = 0x000E
$CMD_ABOUT = 1002  # tray::TRAY_CMD_ABOUT

$script:failed = 0
function Check($name, $ok, $detail) {
    if ($ok) { Write-Host "  PASS: $name（$detail）" }
    else { Write-Host "  FAIL: $name（$detail）"; $script:failed++ }
}

function Count-AboutWindows {
    $script:count = 0
    [AboutWin]::EnumWindows({ param($h, $l)
        $sb = New-Object System.Text.StringBuilder 256
        [AboutWin]::GetClassName($h, $sb, 256) | Out-Null
        if ($sb.ToString() -eq "TimeStickAbout") { $script:count++ }
        return $true
    }, [IntPtr]::Zero) | Out-Null
    return $script:count
}

Write-Host "=== 0. cargo build --release ==="
Push-Location "D:\青山公仔\应用\Code\TimeStick"
cargo build --release
$buildOk = ($LASTEXITCODE -eq 0)
Pop-Location
if (-not $buildOk) { Write-Host "FAIL: 构建失败"; exit 1 }

Write-Host "`n=== 1. 启动（带日志）==="
Stop-Process -Name timestick -Force -ErrorAction SilentlyContinue
Start-Sleep -Milliseconds 500
$env:TIMESTICK_LOG = $log
$p = Start-Process -FilePath $exe -PassThru
Start-Sleep -Seconds 2
# 实例秒退 = 单实例互斥锁被残留实例占用（日志为空是它的指纹），及早报因避免级联失败
if ($null -eq (Get-Process -Id $p.Id -ErrorAction SilentlyContinue)) {
    Write-Host "FAIL: 实例立即退出（单实例互斥锁被残留进程占用？请检查 Get-Process timestick 后重试）"
    Get-Content $log -Encoding UTF8 -ErrorAction SilentlyContinue | Select-Object -Last 10 | ForEach-Object { Write-Host "  $_" }
    exit 1
}
$main = [AboutWin]::FindMain()
if ($main -eq [IntPtr]::Zero) {
    Write-Host "FAIL: 未找到主窗口（TimeStickMain）"
    Stop-Process -Id $p.Id -Force -ErrorAction SilentlyContinue
    exit 1
}
Write-Host "主窗口: $main  PID=$($p.Id)"

Write-Host "`n=== 2. 打开关于对话框（WM_COMMAND 1002）==="
[AboutWin]::PostMessageW($main, $WM_COMMAND, [UIntPtr][uint32]$CMD_ABOUT, [IntPtr]::Zero) | Out-Null
$about = [IntPtr]::Zero
for ($i = 0; $i -lt 30; $i++) {
    $about = [AboutWin]::FindAbout()
    if ($about -ne [IntPtr]::Zero) { break }
    Start-Sleep -Milliseconds 100
}
Check "打开关于对话框" ($about -ne [IntPtr]::Zero) "hwnd=$about"

Write-Host "`n=== 3. 防重入（重复命令不重开）==="
[AboutWin]::PostMessageW($main, $WM_COMMAND, [UIntPtr][uint32]$CMD_ABOUT, [IntPtr]::Zero) | Out-Null
Start-Sleep -Milliseconds 500
$n = Count-AboutWindows
Check "防重入" ($n -eq 1) "TimeStickAbout 窗口数=$n"

Write-Host "`n=== 4. 许可文本非空（两个只读 EDIT）==="
$e1 = [AboutWin]::NextEdit($about, [IntPtr]::Zero)
$e2 = [AboutWin]::NextEdit($about, $e1)
Check "两个 EDIT 控件存在" ($e1 -ne [IntPtr]::Zero -and $e2 -ne [IntPtr]::Zero) "e1=$e1 e2=$e2"
$len1 = if ($e1 -ne [IntPtr]::Zero) { [AboutWin]::SendMessageW($e1, $WM_GETTEXTLENGTH, [IntPtr]::Zero, [IntPtr]::Zero).ToInt64() } else { -1 }
$len2 = if ($e2 -ne [IntPtr]::Zero) { [AboutWin]::SendMessageW($e2, $WM_GETTEXTLENGTH, [IntPtr]::Zero, [IntPtr]::Zero).ToInt64() } else { -1 }
Check "本项目许可证文本非空" ($len1 -gt 0) "字符数=$len1"
Check "第三方许可文本非空" ($len2 -gt 0) "字符数=$len2"

Write-Host "`n=== 5. 无边框（阶段四）：关闭按钮存在 + 客户区=整窗 ==="
$btn = [AboutWin]::FindChild($about, "TimeStickCloseBtn")
Check "自绘关闭按钮存在" ($btn -ne [IntPtr]::Zero) "hwnd=$btn"
$wr = New-Object AboutWin+RECT
$cr = New-Object AboutWin+RECT
[AboutWin]::GetWindowRect($about, [ref]$wr) | Out-Null
[AboutWin]::GetClientRect($about, [ref]$cr) | Out-Null
$winW = $wr.Right - $wr.Left; $winH = $wr.Bottom - $wr.Top
$cliW = $cr.Right - $cr.Left; $cliH = $cr.Bottom - $cr.Top
Check "无边框生效（客户区=整窗）" ($winW -eq $cliW -and $winH -eq $cliH) "window=${winW}x${winH} client=${cliW}x${cliH}"

Write-Host "`n=== 6. 关闭（WM_CLOSE）==="
[AboutWin]::SendMessageW($about, $WM_CLOSE, [IntPtr]::Zero, [IntPtr]::Zero) | Out-Null
$gone = $false
for ($i = 0; $i -lt 30; $i++) {
    if (-not [AboutWin]::IsWindow($about)) { $gone = $true; break }
    Start-Sleep -Milliseconds 100
}
Check "关闭对话框" $gone "窗口已销毁=$gone"

Write-Host "`n=== 7. 关闭后重开（守护 WM_APP_ABOUT_CLOSED 回归）==="
[AboutWin]::PostMessageW($main, $WM_COMMAND, [UIntPtr][uint32]$CMD_ABOUT, [IntPtr]::Zero) | Out-Null
$about2 = [IntPtr]::Zero
for ($i = 0; $i -lt 30; $i++) {
    $about2 = [AboutWin]::FindAbout()
    if ($about2 -ne [IntPtr]::Zero) { break }
    Start-Sleep -Milliseconds 100
}
Check "关闭后重开" ($about2 -ne [IntPtr]::Zero) "hwnd=$about2"
if ($about2 -ne [IntPtr]::Zero) {
    [AboutWin]::SendMessageW($about2, $WM_CLOSE, [IntPtr]::Zero, [IntPtr]::Zero) | Out-Null
}

Write-Host "`n=== 8. 清理 + 日志尾部 ==="
Stop-Process -Name timestick -Force -ErrorAction SilentlyContinue
Start-Sleep -Milliseconds 500
Get-Content $log -Encoding UTF8 -ErrorAction SilentlyContinue | Select-Object -Last 10 | ForEach-Object { Write-Host "  $_" }

Write-Host "`n=== 汇总 ==="
if ($script:failed -eq 0) { Write-Host "全部检查通过"; exit 0 }
else { Write-Host "$($script:failed) 项失败"; exit 1 }
