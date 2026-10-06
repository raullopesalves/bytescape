; Installer hooks (bundle > windows > nsis > installerHooks in tauri.conf.json).
; Adds "Scan with Bytescape" to Explorer's right-click menu for folders, the
; inside of a folder and drives, for the current user; the uninstaller
; removes it. The same keys as src/shell_menu.rs.

!macro BYTESCAPE_MENU KEY
  WriteRegStr HKCU "Software\Classes\${KEY}\shell\Bytescape" "" "Scan with Bytescape"
  WriteRegStr HKCU "Software\Classes\${KEY}\shell\Bytescape" "Icon" "$INSTDIR\${MAINBINARYNAME}.exe"
  WriteRegStr HKCU "Software\Classes\${KEY}\shell\Bytescape\command" "" '"$INSTDIR\${MAINBINARYNAME}.exe" "%V"'
!macroend

!macro NSIS_HOOK_POSTINSTALL
  !insertmacro BYTESCAPE_MENU "Directory"
  !insertmacro BYTESCAPE_MENU "Directory\Background"
  !insertmacro BYTESCAPE_MENU "Drive"
!macroend

!macro NSIS_HOOK_PREUNINSTALL
  DeleteRegKey HKCU "Software\Classes\Directory\shell\Bytescape"
  DeleteRegKey HKCU "Software\Classes\Directory\Background\shell\Bytescape"
  DeleteRegKey HKCU "Software\Classes\Drive\shell\Bytescape"
!macroend
