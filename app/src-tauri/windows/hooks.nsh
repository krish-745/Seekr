; Seekr's additions to the NSIS installer (tauri.nsis.json): the content indexer stopped before its
; files are replaced, the SeekrIndex service, starting hidden at login, and the power-saving GPU.
; The installer runs without administrator rights; only the service asks for them.

!macro NSIS_HOOK_PREINSTALL
  ; The installer closes seekr.exe itself; the indexer it starts may still be reading files.
  nsExec::Exec 'taskkill /F /IM seekr-indexer.exe'
  Pop $0
!macroend

!macro NSIS_HOOK_POSTINSTALL
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Run" "Seekr" '"$INSTDIR\seekr.exe" --hidden'
  WriteRegStr HKCU "Software\Microsoft\DirectX\UserGpuPreferences" "$INSTDIR\seekr.exe" "GpuPreference=1;"
  ; One UAC prompt, and only when the service is missing or this version changes it. Declining
  ; leaves C: on the slower scan Seekr does by itself.
  nsExec::Exec '"$INSTDIR\seekr-svc.exe" current'
  Pop $0
  ${If} $0 != 0
    ExecShellWait "runas" "$INSTDIR\seekr-svc.exe" "install" SW_HIDE
  ${EndIf}
!macroend

!macro NSIS_HOOK_PREUNINSTALL
  nsExec::Exec 'taskkill /F /IM seekr-indexer.exe'
  Pop $0
  ; An update replaces files in place and keeps the service. From this copy, not the one in
  ; Program Files, which couldn't delete itself.
  ${If} $UpdateMode <> 1
  ${AndIf} ${FileExists} "$PROGRAMFILES64\Seekr\seekr-svc.exe"
    ExecShellWait "runas" "$INSTDIR\seekr-svc.exe" "uninstall" SW_HIDE
  ${EndIf}
!macroend

!macro NSIS_HOOK_POSTUNINSTALL
  ; Tauri's uninstaller removes the Run entry; your data in %LOCALAPPDATA%\Seekr and
  ; %APPDATA%\Seekr is kept.
  ${If} $UpdateMode <> 1
    DeleteRegValue HKCU "Software\Microsoft\DirectX\UserGpuPreferences" "$INSTDIR\seekr.exe"
  ${EndIf}
!macroend
