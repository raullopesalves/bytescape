; Installer hooks (bundle > windows > nsis > installerHooks in tauri.conf.json).
; Adds "Scan with Bytescape" to Explorer's right-click menu for folders, the
; inside of a folder and drives, for the current user; the uninstaller
; removes it. The same keys as src/shell_menu.rs. Also installs
; bytescape-cli.exe next to the app, so it updates and uninstalls with it.

!macro BYTESCAPE_MENU KEY
  WriteRegStr HKCU "Software\Classes\${KEY}\shell\Bytescape" "" "Scan with Bytescape"
  WriteRegStr HKCU "Software\Classes\${KEY}\shell\Bytescape" "Icon" "$INSTDIR\${MAINBINARYNAME}.exe"
  WriteRegStr HKCU "Software\Classes\${KEY}\shell\Bytescape\command" "" '"$INSTDIR\${MAINBINARYNAME}.exe" "%V"'
!macroend

!macro NSIS_HOOK_POSTINSTALL
  ; The CLI comes from the same build folder as the app (beforeBuildCommand
  ; in tauri.conf.json builds it; the release workflow builds it per target).
  SetOutPath "$INSTDIR"
  !searchreplace BYTESCAPE_CLI_SRC "${MAINBINARYSRCPATH}" "${MAINBINARYNAME}.exe" "bytescape-cli.exe"
  File "${BYTESCAPE_CLI_SRC}"
  !insertmacro BYTESCAPE_MENU "Directory"
  !insertmacro BYTESCAPE_MENU "Directory\Background"
  !insertmacro BYTESCAPE_MENU "Drive"
!macroend

!macro NSIS_HOOK_PREUNINSTALL
  Delete "$INSTDIR\bytescape-cli.exe"
  DeleteRegKey HKCU "Software\Classes\Directory\shell\Bytescape"
  DeleteRegKey HKCU "Software\Classes\Directory\Background\shell\Bytescape"
  DeleteRegKey HKCU "Software\Classes\Drive\shell\Bytescape"
!macroend
