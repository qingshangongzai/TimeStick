# 构建 TimeStick NSIS 安装包：cargo build --release -> makensis -> target\TimeStick-<版本>-setup.exe
# 用法：powershell -File scripts\build_installer.ps1 [-NsisDir D:\Tools\NSIS]
# 注意：本文件必须保持 UTF-8 带 BOM 编码（PowerShell 5.1 无 BOM 时按 ANSI 解析中文会语法报错）
param(
    [string]$NsisDir = "D:\Tools\NSIS"
)
$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $PSScriptRoot

# 从 Cargo.toml 读取版本号（与 exe 版本保持一致）
$version = (Select-String -Path "$root\Cargo.toml" -Pattern '^version = "([^"]+)"').Matches[0].Groups[1].Value

# 确保 release 构建存在
Push-Location $root
cargo build --release
if ($LASTEXITCODE -ne 0) { Pop-Location; exit $LASTEXITCODE }
Pop-Location

# 编译安装包
$makensis = Join-Path $NsisDir "makensis.exe"
if (-not (Test-Path $makensis)) {
    Write-Error "找不到 makensis.exe：$makensis（请用 -NsisDir 指定 NSIS 安装目录）"
}
Push-Location $root
& $makensis "/DVERSION=$version" "installer.nsi"
if ($LASTEXITCODE -ne 0) { Pop-Location; exit $LASTEXITCODE }
Pop-Location

Write-Host "`n安装包已生成：$root\target\TimeStick-$version-setup.exe"
