; NSIS installer for Rockchip Universal Imager
; Expects STAGE_DIR (absolute) and OUT_EXE to be defined via /D on the command line.
; Layout in STAGE_DIR:
;   rockchip-universal-imager.exe
;   rkdeveloptool.exe
;   loader_binaries\...

!ifndef STAGE_DIR
  !error "Pass /DSTAGE_DIR=... pointing at the staged install folder"
!endif
!ifndef OUT_EXE
  !error "Pass /DOUT_EXE=... for the output installer path"
!endif

Unicode true
RequestExecutionLevel admin

Name "Rockchip Universal Imager"
OutFile "${OUT_EXE}"
InstallDir "$PROGRAMFILES64\Rockchip Universal Imager"
InstallDirRegKey HKLM "Software\Rockchip Universal Imager" "InstallDir"

!include "MUI2.nsh"

!define MUI_ABORTWARNING
!insertmacro MUI_PAGE_WELCOME
!insertmacro MUI_PAGE_DIRECTORY
!insertmacro MUI_PAGE_INSTFILES
!insertmacro MUI_PAGE_FINISH
!insertmacro MUI_UNPAGE_CONFIRM
!insertmacro MUI_UNPAGE_INSTFILES
!insertmacro MUI_LANGUAGE "English"

Section "Install"
  SetOutPath "$INSTDIR"
  File /r "${STAGE_DIR}\*.*"

  WriteRegStr HKLM "Software\Rockchip Universal Imager" "InstallDir" "$INSTDIR"
  WriteUninstaller "$INSTDIR\Uninstall.exe"

  CreateDirectory "$SMPROGRAMS\Rockchip Universal Imager"
  CreateShortCut "$SMPROGRAMS\Rockchip Universal Imager\Rockchip Universal Imager.lnk" \
    "$INSTDIR\rockchip-universal-imager.exe"
  CreateShortCut "$DESKTOP\Rockchip Universal Imager.lnk" \
    "$INSTDIR\rockchip-universal-imager.exe"

  WriteRegStr HKLM "Software\Microsoft\Windows\CurrentVersion\Uninstall\RockchipUniversalImager" \
    "DisplayName" "Rockchip Universal Imager"
  WriteRegStr HKLM "Software\Microsoft\Windows\CurrentVersion\Uninstall\RockchipUniversalImager" \
    "UninstallString" "$INSTDIR\Uninstall.exe"
  WriteRegStr HKLM "Software\Microsoft\Windows\CurrentVersion\Uninstall\RockchipUniversalImager" \
    "InstallLocation" "$INSTDIR"
  WriteRegDWORD HKLM "Software\Microsoft\Windows\CurrentVersion\Uninstall\RockchipUniversalImager" \
    "NoModify" 1
  WriteRegDWORD HKLM "Software\Microsoft\Windows\CurrentVersion\Uninstall\RockchipUniversalImager" \
    "NoRepair" 1
SectionEnd

Section "Uninstall"
  Delete "$SMPROGRAMS\Rockchip Universal Imager\Rockchip Universal Imager.lnk"
  RMDir "$SMPROGRAMS\Rockchip Universal Imager"
  Delete "$DESKTOP\Rockchip Universal Imager.lnk"
  Delete "$INSTDIR\Uninstall.exe"
  RMDir /r "$INSTDIR"
  DeleteRegKey HKLM "Software\Rockchip Universal Imager"
  DeleteRegKey HKLM "Software\Microsoft\Windows\CurrentVersion\Uninstall\RockchipUniversalImager"
SectionEnd
