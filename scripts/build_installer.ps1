# 构建 TimeStick NSIS 安装包：cargo build --release -> makensis -> target\TimeStick-<版本>-setup.exe
# 用法：powershell -File scripts\build_installer.ps1 [-NsisDir <NSIS目录>]
# NSIS 定位顺序：-NsisDir > PATH > 注册表默认值 > Program Files 约定目录；要求 3.x（installer.nsi 使用 Unicode true）
# 注意：本文件必须保持 UTF-8 带 BOM 编码（PowerShell 5.1 无 BOM 时按 ANSI 解析中文会语法报错）
param(
    [string]$NsisDir = ""
)
$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $PSScriptRoot

# 解析 makensis.exe 路径；都没有返回 $null
function Resolve-Makensis([string]$Dir) {
    if ($Dir) {
        # 显式指定就必须有效：不静默回落到其他 NSIS，否则可能用的不是用户以为的那个版本
        $p = Join-Path $Dir "makensis.exe"
        if (-not (Test-Path $p)) { throw "指定的 -NsisDir 下没有 makensis.exe：$p" }
        return $p
    }
    $cmd = Get-Command makensis.exe -ErrorAction SilentlyContinue
    if ($cmd) { return $cmd.Source }
    # NSIS 3.x 安装器把安装目录写在键的默认值里（实测 3.13），不是 InstallDir 值名
    foreach ($key in 'HKLM:\SOFTWARE\WOW6432Node\NSIS', 'HKLM:\SOFTWARE\NSIS') {
        $item = Get-Item $key -ErrorAction SilentlyContinue
        if (-not $item) { continue }
        $installed = $item.GetValue('')
        if (-not $installed) { continue }
        $p = Join-Path $installed "makensis.exe"
        if (Test-Path $p) { return $p }
    }
    # 注册表被清理过或绿色安装的兜底（官方安装器的默认落点）
    foreach ($dir in "${env:ProgramFiles(x86)}\NSIS", "$env:ProgramFiles\NSIS") {
        $p = Join-Path $dir "makensis.exe"
        if (Test-Path $p) { return $p }
    }
    return $null
}

# 从 Cargo.toml 的 [package] 段读取版本号：限定段名，避免日后插入 [workspace.package] 或依赖 version 时取错
$inPackage = $false
$version = $null
foreach ($line in Get-Content "$root\Cargo.toml") {
    if ($line -match '^\s*\[(.+)\]') { $inPackage = $Matches[1] -eq 'package'; continue }
    if ($inPackage -and $line -match '^version\s*=\s*"([^"]+)"') { $version = $Matches[1]; break }
}
if (-not $version) { throw "未在 Cargo.toml 的 [package] 段找到 version" }

$makensis = Resolve-Makensis $NsisDir
if (-not $makensis) {
    Write-Host "错误：未找到 makensis.exe。已探测：-NsisDir / PATH / 注册表 / Program Files" -ForegroundColor Red
    Write-Host "  安装：winget install -e --id NSIS.NSIS   或   choco install nsis -y"
    Write-Host "  非默认安装：powershell -File scripts\build_installer.ps1 -NsisDir <NSIS目录>"
    exit 1
}

# 版本闸门：installer.nsi 使用 Unicode true，NSIS 2.x 编译失败且报错不直观
$versionText = (& $makensis /VERSION) -join ''
if ($versionText -notmatch 'v(\d+)\.') { throw "无法识别 makensis 版本：$versionText" }
if ([int]$Matches[1] -lt 3) { throw "NSIS 版本过低（$versionText）：installer.nsi 使用 Unicode true，需要 3.x" }
Write-Host "使用 makensis：$makensis（$versionText）"

# 确保 release 构建存在（增量执行，已构建时为无操作）
Push-Location $root
cargo build --release
if ($LASTEXITCODE -ne 0) { Pop-Location; exit $LASTEXITCODE }
Pop-Location

# 编译安装包
Push-Location $root
& $makensis "/DVERSION=$version" "installer.nsi"
if ($LASTEXITCODE -ne 0) { Pop-Location; exit $LASTEXITCODE }
Pop-Location

Write-Host "`n安装包已生成：$root\target\TimeStick-$version-setup.exe"
