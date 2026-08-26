; TimeStick（时间贴）NSIS 安装脚本
; 构建：cargo build --release && "D:\Tools\NSIS\makensis.exe" installer.nsi
;      （版本号可在命令行覆盖：makensis /DVERSION=x.y.z installer.nsi）
; 产物：target\TimeStick-<版本>-setup.exe
; 注意：本文件必须保持 UTF-8 带 BOM 编码（NSIS 无 BOM 时按 ANSI 解析，中文会乱码）

Unicode true
!include "MUI2.nsh"

; ---------- 产品元信息（与 Cargo.toml 同步维护） ----------
!ifndef VERSION
  !define VERSION "0.1.0"
!endif
!define PRODUCT_NAME   "TimeStick"
!define PRODUCT_ZH     "时间贴"
!define PUBLISHER      "青山公仔"
!define APP_EXE        "timestick.exe"
!define APP_ICO        "assets\icon.ico"
!define UNINST_KEY     "Software\Microsoft\Windows\CurrentVersion\Uninstall\TimeStick"

; 安装包 / 卸载器版本信息（资源管理器属性可见；图标由下方 MUI_ICON/MUI_UNICON 接管）
VIProductVersion "${VERSION}.0"
VIAddVersionKey "ProductName" "${PRODUCT_NAME}"
VIAddVersionKey "FileDescription" "${PRODUCT_ZH} 安装程序"
VIAddVersionKey "CompanyName" "${PUBLISHER}"
VIAddVersionKey "LegalCopyright" "© ${PUBLISHER}"
VIAddVersionKey "FileVersion" "${VERSION}"
VIAddVersionKey "ProductVersion" "${VERSION}"

; ---------- 安装包基本配置 ----------
Name "${PRODUCT_ZH}"
OutFile "target\TimeStick-${VERSION}-setup.exe"
InstallDir "$PROGRAMFILES64\TimeStick"
InstallDirRegKey HKLM "${UNINST_KEY}" "InstallLocation"
RequestExecutionLevel admin
SetCompressor /SOLID lzma

; ---------- 界面（简体中文，安装完成可勾选立即运行） ----------
!define MUI_ICON "${APP_ICO}"
!define MUI_UNICON "${APP_ICO}"
!define MUI_ABORTWARNING
!define MUI_FINISHPAGE_RUN "$INSTDIR\${APP_EXE}"
!define MUI_FINISHPAGE_RUN_TEXT "立即运行 时间贴"
!insertmacro MUI_PAGE_WELCOME
!insertmacro MUI_PAGE_DIRECTORY
!insertmacro MUI_PAGE_INSTFILES
!insertmacro MUI_PAGE_FINISH
!insertmacro MUI_UNPAGE_CONFIRM
!insertmacro MUI_UNPAGE_INSTFILES
!insertmacro MUI_LANGUAGE "SimpChinese"

; ---------- 安装 ----------
Section "安装" SecMain
  ; perMachine 安装：32 位安装器也直接读写 64 位注册表视图（避免 WOW6432Node 重定向），
  ; 开始菜单快捷方式放公共位置（All Users），卸载段必须对称设置
  SetRegView 64
  SetShellVarContext all
  ; 先结束运行中的实例（覆盖安装时 exe 被占用无法写入；未运行时 taskkill 空跑无害）
  nsExec::ExecToStack 'taskkill /F /IM "${APP_EXE}"'
  SetOutPath "$INSTDIR"
  File "target\release\${APP_EXE}"

  ; 开始菜单快捷方式
  CreateDirectory "$SMPROGRAMS\${PRODUCT_NAME}"
  CreateShortCut "$SMPROGRAMS\${PRODUCT_NAME}\${PRODUCT_ZH}.lnk" "$INSTDIR\${APP_EXE}" "" "$INSTDIR\${APP_EXE}" 0 SW_SHOWNORMAL "" "全屏时间悬浮窗：进入全屏时在右下角显示时间"

  ; 卸载器
  WriteUninstaller "$INSTDIR\uninstall.exe"

  ; 控制面板「程序和功能」卸载条目
  WriteRegStr   HKLM "${UNINST_KEY}" "DisplayName" "${PRODUCT_ZH}"
  WriteRegStr   HKLM "${UNINST_KEY}" "DisplayIcon" "$INSTDIR\${APP_EXE}"
  WriteRegStr   HKLM "${UNINST_KEY}" "DisplayVersion" "${VERSION}"
  WriteRegStr   HKLM "${UNINST_KEY}" "Publisher" "${PUBLISHER}"
  WriteRegStr   HKLM "${UNINST_KEY}" "InstallLocation" "$INSTDIR"
  WriteRegStr   HKLM "${UNINST_KEY}" "UninstallString" '"$INSTDIR\uninstall.exe"'
  WriteRegStr   HKLM "${UNINST_KEY}" "QuietUninstallString" '"$INSTDIR\uninstall.exe" /S'
  WriteRegDWORD HKLM "${UNINST_KEY}" "NoModify" 1
  WriteRegDWORD HKLM "${UNINST_KEY}" "NoRepair" 1
  WriteRegDWORD HKLM "${UNINST_KEY}" "EstimatedSize" 200 ; 安装体积估算(KB)：exe 约 170KB + 卸载器，改版后酌情同步
SectionEnd

; ---------- 卸载 ----------
Section "Uninstall"
  ; 与安装段对称：64 位注册表视图 + 公共开始菜单位置
  SetRegView 64
  SetShellVarContext all
  ; 先结束运行中的实例（单实例程序，否则 exe 被占用无法删除）
  nsExec::ExecToStack 'taskkill /F /IM "${APP_EXE}"'

  Delete "$INSTDIR\${APP_EXE}"
  Delete "$INSTDIR\uninstall.exe"
  RMDir  "$INSTDIR"

  Delete "$SMPROGRAMS\${PRODUCT_NAME}\${PRODUCT_ZH}.lnk"
  RMDir  "$SMPROGRAMS\${PRODUCT_NAME}"

  DeleteRegKey HKLM "${UNINST_KEY}"
SectionEnd
