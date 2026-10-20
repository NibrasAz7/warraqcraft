; WarraqCraft Windows installer — per-user install, no admin required.
; Build:  makensis /DVERSION=0.1.0 /DSRCDIR=dist\warraqcraft-0.1.0-windows-x64 scripts\installer.nsi

Unicode true
!include "MUI2.nsh"

!ifndef VERSION
!define VERSION "0.0.0"
!endif
!ifndef SRCDIR
!define SRCDIR "${__FILEDIR__}\..\dist\warraqcraft-${VERSION}-windows-x64"
!endif
!ifndef OUTFILE
!define OUTFILE "${__FILEDIR__}\..\dist\warraqcraft-${VERSION}-windows-x64-setup.exe"
!endif

Name "WarraqCraft ${VERSION}"
OutFile "${OUTFILE}"
InstallDir "$LOCALAPPDATA\Programs\WarraqCraft"
RequestExecutionLevel user
SetCompressor /SOLID lzma
ShowInstDetails show
ShowUninstDetails show

!define MUI_ABORTWARNING
!insertmacro MUI_PAGE_WELCOME
!insertmacro MUI_PAGE_LICENSE "${SRCDIR}\LICENSE-MIT"
!insertmacro MUI_PAGE_DIRECTORY
!insertmacro MUI_PAGE_COMPONENTS
!insertmacro MUI_PAGE_INSTFILES
!define MUI_FINISHPAGE_RUN "$INSTDIR\warraqcraft.exe"
!define MUI_FINISHPAGE_RUN_TEXT "Launch WarraqCraft"
!insertmacro MUI_PAGE_FINISH

!insertmacro MUI_UNPAGE_CONFIRM
!insertmacro MUI_UNPAGE_INSTFILES

!insertmacro MUI_LANGUAGE "English"

Section "WarraqCraft (required)" SecMain
    SectionIn RO
    SetOutPath "$INSTDIR"
    SetOverwrite on
    File /r "${SRCDIR}\*.*"

    WriteUninstaller "$INSTDIR\uninstall.exe"

    CreateDirectory "$SMPROGRAMS\WarraqCraft"
    CreateShortcut "$SMPROGRAMS\WarraqCraft\WarraqCraft.lnk" "$INSTDIR\warraqcraft.exe"
    CreateShortcut "$SMPROGRAMS\WarraqCraft\Uninstall WarraqCraft.lnk" "$INSTDIR\uninstall.exe"

    ; Offer WarraqCraft in the .tex "Open with" list without stealing the
    ; file association — the user keeps their default editor.
    WriteRegStr HKCU "Software\Classes\.tex\OpenWithProgids" "WarraqCraft.tex" ""
    WriteRegStr HKCU "Software\Classes\WarraqCraft.tex" "" "LaTeX document"
    WriteRegStr HKCU "Software\Classes\WarraqCraft.tex\shell\open\command" "" '"$INSTDIR\warraqcraft.exe" "%1"'

    WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\WarraqCraft" "DisplayName" "WarraqCraft"
    WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\WarraqCraft" "DisplayVersion" "${VERSION}"
    WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\WarraqCraft" "Publisher" "WarraqCraft"
    WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\WarraqCraft" "InstallLocation" "$INSTDIR"
    WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\WarraqCraft" "UninstallString" '"$INSTDIR\uninstall.exe"'
    WriteRegDWORD HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\WarraqCraft" "NoModify" 1
    WriteRegDWORD HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\WarraqCraft" "NoRepair" 1
SectionEnd

Section "Desktop shortcut" SecDesktop
    CreateShortcut "$DESKTOP\WarraqCraft.lnk" "$INSTDIR\warraqcraft.exe"
SectionEnd

LangString DESC_SecMain ${LANG_ENGLISH} "WarraqCraft application, command-line tool, bundled Tectonic engine and templates."
LangString DESC_SecDesktop ${LANG_ENGLISH} "Create a WarraqCraft shortcut on the desktop."
!insertmacro MUI_FUNCTION_DESCRIPTION_BEGIN
    !insertmacro MUI_DESCRIPTION_TEXT ${SecMain} $(DESC_SecMain)
    !insertmacro MUI_DESCRIPTION_TEXT ${SecDesktop} $(DESC_SecDesktop)
!insertmacro MUI_FUNCTION_DESCRIPTION_END

Section "Uninstall"
    Delete "$DESKTOP\WarraqCraft.lnk"
    Delete "$SMPROGRAMS\WarraqCraft\WarraqCraft.lnk"
    Delete "$SMPROGRAMS\WarraqCraft\Uninstall WarraqCraft.lnk"
    RMDir "$SMPROGRAMS\WarraqCraft"

    DeleteRegValue HKCU "Software\Classes\.tex\OpenWithProgids" "WarraqCraft.tex"
    DeleteRegKey HKCU "Software\Classes\WarraqCraft.tex"

    DeleteRegKey HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\WarraqCraft"

    RMDir /r "$INSTDIR"
SectionEnd
