; McloudClient 安装器（NSIS 3.x，MUI2；本机已全局默认简体中文）。
; 用法：makensis /V2 scripts\installer.nsi（由 scripts/build-installer.sh 调用）。
; 安装内容：三个二进制 + LICENSE；创建快照种子；注册 McloudCore 服务（auto + 崩溃重启）；
; 开始菜单/桌面快捷方式。卸载：停服务 + 删文件 + 删快捷方式（保留 ProgramData 数据）。

!include "MUI2.nsh"

Name "McloudClient"
OutFile "..\target\installer\McloudClient-Setup-0.1.0.exe"
InstallDir "$PROGRAMFILES64\McloudClient"
RequestExecutionLevel admin
Unicode true

!define MUI_ABORTWARNING
!insertmacro MUI_PAGE_WELCOME
!insertmacro MUI_PAGE_DIRECTORY
!insertmacro MUI_PAGE_INSTFILES
!insertmacro MUI_PAGE_FINISH
!insertmacro MUI_UNPAGE_CONFIRM
!insertmacro MUI_UNPAGE_INSTFILES
!insertmacro MUI_LANGUAGE "SimpChinese"
!insertmacro MUI_LANGUAGE "English"

Section "Install"
  ReadEnvStr $R9 PROGRAMDATA
  ; 更新场景：先停服务与旧 GUI，避免覆盖被占用文件。
  nsExec::ExecToLog 'sc.exe stop McloudCore'
  nsExec::ExecToLog 'taskkill /IM client-gui.exe /F'
  ; 兜底释放数据面端口：非服务形态的内核（开发 CLI）或卡死的服务不会响应 sc stop。
  nsExec::ExecToLog 'taskkill /IM client-core.exe /F'
  Sleep 1500
  ; FA 规格二：覆盖前备份上一版二进制到 previous\（回滚来源，离线可回滚）。
  ; 仅当已存在旧版本时备份；首次安装无旧文件，CopyFiles 失败可忽略。
  CreateDirectory "$INSTDIR\previous"
  CopyFiles /SILENT "$INSTDIR\client-core.exe" "$INSTDIR\previous\client-core.exe"
  CopyFiles /SILENT "$INSTDIR\client-gui.exe" "$INSTDIR\previous\client-gui.exe"
  CopyFiles /SILENT "$INSTDIR\sidecar.exe" "$INSTDIR\previous\sidecar.exe"
  SetOutPath "$INSTDIR"
  File "..\target\release\client-core.exe"
  File "..\target\release\client-gui.exe"
  File "..\target\release\sidecar.exe"
  File "..\LICENSE"
  SetOutPath "$INSTDIR\assets"
  File "..\crates\client-gui\assets\icon.ico"
  SetOutPath "$INSTDIR"

  ; Windows App Runtime（reactor GUI 依赖）检查：缺失则提示并中止。
  nsExec::ExecToStack '$WINDIR\Sysnative\WindowsPowerShell\v1.0\powershell.exe -NoProfile -Command "if (Get-AppxPackage Microsoft.WindowsAppRuntime.1.* ) { exit 0 } else { exit 1 }"'
  Pop $0
  Pop $1
  ${If} $0 != 0
    MessageBox MB_OK|MB_ICONSTOP "缺少 Windows App Runtime（WinUI 依赖）。请先安装：https://learn.microsoft.com/windows/apps/windows-app-sdk/downloads"
    Abort
  ${EndIf}

  ; 快照种子：内核绝不以空配置启动（与 install-service.sh 同口径）。
  CreateDirectory "$R9\McloudClient\snapshots"
  CreateDirectory "$R9\McloudClient\logs"
  ${IfNot} ${FileExists} "$R9\McloudClient\snapshots\live.json"
    FileOpen $0 "$R9\McloudClient\snapshots\live.json" w
    FileWrite $0 '{"proxies":[],"proxy-groups":[],"rules":["MATCH,DIRECT"]}'
    FileClose $0
  ${EndIf}
  ${IfNot} ${FileExists} "$R9\McloudClient\snapshots\current.txt"
    CopyFiles "$R9\McloudClient\snapshots\live.json" "$R9\McloudClient\snapshots\0000.json"
    FileOpen $0 "$R9\McloudClient\snapshots\current.txt" w
    FileWrite $0 "0"
    FileClose $0
  ${EndIf}

  ; 服务：binPath 全绝对路径（SCM 工作目录为 System32）。
  nsExec::ExecToLog 'sc.exe create McloudCore binPath= "\"$INSTDIR\client-core.exe\" --service --profile \"$R9\McloudClient\snapshots\live.json\" --ipc-pipe \\.\pipe\mcloud-client --log-dir \"$R9\McloudClient\logs\"" start= auto DisplayName= "McloudClient Core"'
  nsExec::ExecToLog 'sc.exe description McloudCore "McloudClient data plane (VLESS/WS/TLS/mlkem)"'
  nsExec::ExecToLog 'sc.exe failure McloudCore reset= 86400 actions= restart/5000/restart/10000/restart/30000'

  CreateDirectory "$SMPROGRAMS\McloudClient"
  CreateShortcut "$SMPROGRAMS\McloudClient\McloudClient.lnk" "$INSTDIR\client-gui.exe" '--pipe \\.\pipe\mcloud-client'
  CreateShortcut "$SMPROGRAMS\McloudClient\Uninstall.lnk" "$INSTDIR\Uninstall.exe"
  CreateShortcut "$DESKTOP\McloudClient.lnk" "$INSTDIR\client-gui.exe" '--pipe \\.\pipe\mcloud-client'

  WriteUninstaller "$INSTDIR\Uninstall.exe"
  WriteRegStr HKLM "Software\Microsoft\Windows\CurrentVersion\Uninstall\McloudClient" "DisplayName" "McloudClient"
  WriteRegStr HKLM "Software\Microsoft\Windows\CurrentVersion\Uninstall\McloudClient" "UninstallString" "$\"$INSTDIR\Uninstall.exe$\""
  WriteRegStr HKLM "Software\Microsoft\Windows\CurrentVersion\Uninstall\McloudClient" "DisplayVersion" "0.1.0"
  WriteRegStr HKLM "Software\Microsoft\Windows\CurrentVersion\Uninstall\McloudClient" "Publisher" "Mcloud136"
  ; 服务由拉起的 GUI 补启动（长生命周期进程，避免安装器退出竞态）。
  Exec '"$INSTDIR\client-gui.exe" --pipe \\.\pipe\mcloud-client'
SectionEnd

Section "Uninstall"
  nsExec::ExecToLog 'sc.exe stop McloudCore'
  Sleep 2000
  nsExec::ExecToLog 'sc.exe delete McloudCore'
  Delete "$DESKTOP\McloudClient.lnk"
  Delete "$SMPROGRAMS\McloudClient\McloudClient.lnk"
  Delete "$SMPROGRAMS\McloudClient\Uninstall.lnk"
  RMDir "$SMPROGRAMS\McloudClient"
  Delete "$INSTDIR\client-core.exe"
  Delete "$INSTDIR\client-gui.exe"
  Delete "$INSTDIR\sidecar.exe"
  Delete "$INSTDIR\LICENSE"
  Delete "$INSTDIR\assets\icon.ico"
  RMDir "$INSTDIR\assets"
  Delete "$INSTDIR\Uninstall.exe"
  RMDir "$INSTDIR"
  DeleteRegKey HKLM "Software\Microsoft\Windows\CurrentVersion\Uninstall\McloudClient"
  ; 保留 $R9\McloudClient（快照/日志），避免误删用户数据。
SectionEnd