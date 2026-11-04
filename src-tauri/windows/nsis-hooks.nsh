; MultiCLI Workbench - NSIS installer hooks.
;
; Directory layout this file protects:
;   install dir : %LOCALAPPDATA%\MultiCLI Workbench   (bundle product name)
;   data dir    : %LOCALAPPDATA%\MultiCLI\<profile>   (dev/test/prod, app-owned)
;
; The stock Tauri uninstaller removes %LOCALAPPDATA%\<bundle id> only when the
; user ticks "delete application data". That path is not this application's
; data directory, so the checkbox would silently do nothing. This hook points
; it at the real directory. The checkbox is unchecked by default, so an
; ordinary uninstall keeps every profile, backup and diagnostic file.
;
; The guard against $INSTDIR is defensive: if a future bundle configuration
; ever installs into the data directory, the optional cleanup must not wipe the
; binaries that are still being uninstalled.

!macro NSIS_HOOK_POSTUNINSTALL
  ${If} $DeleteAppDataCheckboxState = 1
  ${AndIf} $UpdateMode <> 1
  ${AndIf} "$LOCALAPPDATA" != ""
    ; Never delete when the install directory is the data directory itself or
    ; lives inside it: RMDir /r on the parent would remove the running binaries.
    StrCpy $0 "$LOCALAPPDATA\MultiCLI"
    StrLen $1 "$0\"
    StrCpy $2 "$INSTDIR" $1
    ${If} "$2" != "$0\"
    ${AndIf} "$INSTDIR" != "$0"
      RMDir /r "$0"
    ${EndIf}
  ${EndIf}
!macroend
