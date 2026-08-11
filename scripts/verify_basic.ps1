# TimeStick 运行时验证脚本（阶段1：依赖/启动/单实例/内存）
$ErrorActionPreference = "Continue"
$exe = "D:\青山公仔\应用\Code\TimeStick\target\release\timestick.exe"
$log = "$env:TEMP\timestick_test.log"
Remove-Item $log -ErrorAction SilentlyContinue

Write-Host "=== 1. PE 导入的 DLL（确认无第三方依赖）==="
$bytes = [System.IO.File]::ReadAllBytes($exe)
$peOffset = [BitConverter]::ToInt32($bytes, 0x3C)
$numSections = [BitConverter]::ToUInt16($bytes, $peOffset + 6)
$optHeaderSize = [BitConverter]::ToUInt16($bytes, $peOffset + 20)
$optHeader = $peOffset + 24
$magic = [BitConverter]::ToUInt16($bytes, $optHeader)
$ddOffset = if ($magic -eq 0x20b) { $optHeader + 112 } else { $optHeader + 96 }  # PE32+ / PE32
$importRva = [BitConverter]::ToUInt32($bytes, $ddOffset + 8)  # IMPORT table RVA (2nd entry: offset 8)
# 简单方式：搜索字节序列不可靠，改用列出所有节中可打印 DLL 名太复杂；用 Section 解析
$sectionTable = $optHeader + $optHeaderSize
$imports = @()
for ($i = 0; $i -lt $numSections; $i++) {
    $base = $sectionTable + $i * 40
    $va = [BitConverter]::ToUInt32($bytes, $base + 12)
    $raw = [BitConverter]::ToUInt32($bytes, $base + 20)
    $size = [BitConverter]::ToUInt32($bytes, $base + 16)
    if ($importRva -ge $va -and $importRva -lt ($va + $size)) {
        $offset = $raw + ($importRva - $va)
        $p = $offset
        while ($true) {
            $nameRva = [BitConverter]::ToUInt32($bytes, $p + 12)
            if ($nameRva -eq 0) { break }
            $no = $raw + ($nameRva - $va)
            $end = $no
            while ($bytes[$end] -ne 0) { $end++ }
            $imports += [System.Text.Encoding]::ASCII.GetString($bytes, $no, $end - $no)
            $p += 20
        }
        break
    }
}
$imports | ForEach-Object { Write-Host "  $_" }

Write-Host "`n=== 2. 启动（带日志）==="
$env:TIMESTICK_LOG = $log
$p = Start-Process -FilePath $exe -PassThru
Start-Sleep -Seconds 2
$proc = Get-Process -Id $p.Id -ErrorAction SilentlyContinue
if ($proc) {
    Write-Host "  进程存活 PID=$($p.Id) 私有内存=$([math]::Round($proc.PrivateMemorySize64/1MB,2))MB 工作集=$([math]::Round($proc.WorkingSet64/1MB,2))MB"
} else {
    Write-Host "  进程已退出（启动失败）"
}

Write-Host "`n=== 3. 单实例（二次启动应静默退出）==="
$p2 = Start-Process -FilePath $exe -PassThru
Start-Sleep -Seconds 2
$proc2 = Get-Process -Id $p2.Id -ErrorAction SilentlyContinue
if ($proc2) { Write-Host "  失败：第二个实例仍在运行 PID=$($p2.Id)" } else { Write-Host "  通过：第二个实例自动退出" }

Write-Host "`n=== 4. 日志内容 ==="
Start-Sleep -Seconds 1
Get-Content $log -ErrorAction SilentlyContinue | ForEach-Object { Write-Host "  $_" }

Write-Host "`n=== 5. 清理（结束测试进程）==="
Stop-Process -Name timestick -Force -ErrorAction SilentlyContinue
Start-Sleep -Milliseconds 500
Write-Host "  已停止"
