; RemoteFriend Windows installer (per user, no administrator rights needed).
; Build: makensis /DVERSION=x.y.z /DSRC=<folder with exes> remotefriend.nsi

Unicode True
!include "MUI2.nsh"

!ifndef VERSION
  !define VERSION "0.0.0"
!endif
!ifndef SRC
  !define SRC "..\..\target\release"
!endif

Name "RemoteFriend"
OutFile "RemoteFriend-Setup.exe"
InstallDir "$LOCALAPPDATA\Programs\RemoteFriend"
RequestExecutionLevel user
BrandingText "RemoteFriend ${VERSION}"

!define MUI_ICON "..\..\assets\icon.ico"
!define MUI_UNICON "..\..\assets\icon.ico"
!define MUI_FINISHPAGE_RUN "$INSTDIR\remotefriend.exe"
!define MUI_FINISHPAGE_RUN_TEXT "Start RemoteFriend"

!insertmacro MUI_PAGE_WELCOME
!insertmacro MUI_PAGE_INSTFILES
!insertmacro MUI_PAGE_FINISH
!insertmacro MUI_UNPAGE_CONFIRM
!insertmacro MUI_UNPAGE_INSTFILES
!insertmacro MUI_LANGUAGE "English"

!define UNINST_KEY "Software\Microsoft\Windows\CurrentVersion\Uninstall\RemoteFriend"

Section "RemoteFriend"
  ; An update must replace files of a running copy.
  nsExec::Exec 'taskkill /IM remotefriend.exe /F'
  Sleep 500
  SetOutPath "$INSTDIR"
  File "${SRC}\remotefriend.exe"
  File "${SRC}\remote-friend-host.exe"
  File "..\..\assets\icon.ico"
  CreateShortcut "$SMPROGRAMS\RemoteFriend.lnk" "$INSTDIR\remotefriend.exe" "" "$INSTDIR\icon.ico"
  CreateShortcut "$DESKTOP\RemoteFriend.lnk" "$INSTDIR\remotefriend.exe" "" "$INSTDIR\icon.ico"
  WriteUninstaller "$INSTDIR\Uninstall.exe"
  WriteRegStr HKCU "${UNINST_KEY}" "DisplayName" "RemoteFriend"
  WriteRegStr HKCU "${UNINST_KEY}" "DisplayVersion" "${VERSION}"
  WriteRegStr HKCU "${UNINST_KEY}" "Publisher" "RemoteFriend"
  WriteRegStr HKCU "${UNINST_KEY}" "DisplayIcon" "$INSTDIR\icon.ico"
  WriteRegStr HKCU "${UNINST_KEY}" "InstallLocation" "$INSTDIR"
  WriteRegStr HKCU "${UNINST_KEY}" "UninstallString" '"$INSTDIR\Uninstall.exe"'
  WriteRegDWORD HKCU "${UNINST_KEY}" "NoModify" 1
  WriteRegDWORD HKCU "${UNINST_KEY}" "NoRepair" 1
SectionEnd

Section "Uninstall"
  nsExec::Exec 'taskkill /IM remotefriend.exe /F'
  Sleep 500
  Delete "$INSTDIR\remotefriend.exe"
  Delete "$INSTDIR\remote-friend-host.exe"
  Delete "$INSTDIR\icon.ico"
  Delete "$INSTDIR\Uninstall.exe"
  RMDir "$INSTDIR"
  Delete "$SMPROGRAMS\RemoteFriend.lnk"
  Delete "$DESKTOP\RemoteFriend.lnk"
  DeleteRegValue HKCU "Software\Microsoft\Windows\CurrentVersion\Run" "RemoteFriend"
  DeleteRegKey HKCU "${UNINST_KEY}"
SectionEnd
