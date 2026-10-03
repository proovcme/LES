; A dedicated public-edition installer; never invokes full LES setup helpers.
Unicode True
RequestExecutionLevel user
!include MUI2.nsh
!include LogicLib.nsh
!include nsDialogs.nsh
!ifndef PAYLOAD_DIR
  !error "Pass /DPAYLOAD_DIR with a verified complete LES Light payload"
!endif
!ifndef OUTPUT_FILE
  !error "Pass /DOUTPUT_FILE"
!endif
!ifndef PRODUCT_VERSION
  !error "Pass /DPRODUCT_VERSION from config/version.json"
!endif
Name "LES RAG"
Caption "LES RAG ${PRODUCT_VERSION}"
VIProductVersion "${PRODUCT_VERSION}.0"
VIAddVersionKey "ProductName" "LES RAG"
VIAddVersionKey "ProductVersion" "${PRODUCT_VERSION}"
VIAddVersionKey "FileDescription" "Установка LES RAG"
VIAddVersionKey "FileVersion" "${PRODUCT_VERSION}"
VIAddVersionKey "LegalCopyright" "OVC / LES"
OutFile "${OUTPUT_FILE}"
InstallDir "$LOCALAPPDATA\Programs\LES Light"
; Per-file compression avoids a second full uncompressed solid archive in TEMP.
SetCompressor zlib
Var RemoveData
Var RemoveDataCheckbox
Var HelperExit

Function .onInit
  StrCpy $INSTDIR "$LOCALAPPDATA\Programs\LES Light"
FunctionEnd

!insertmacro MUI_PAGE_WELCOME
!insertmacro MUI_PAGE_INSTFILES
!insertmacro MUI_PAGE_FINISH
!insertmacro MUI_UNPAGE_CONFIRM
UninstPage custom un.DataPage un.DataPageLeave
!insertmacro MUI_UNPAGE_INSTFILES
!insertmacro MUI_LANGUAGE "Russian"

Section "LES RAG"
  InitPluginsDir
  SetOutPath "$PLUGINSDIR"
  File "ensure-webview.ps1"
  DetailPrint "Проверка компонентов окна LES RAG..."
  nsExec::ExecToLog 'powershell.exe -NoProfile -NonInteractive -ExecutionPolicy Bypass -File "$PLUGINSDIR\ensure-webview.ps1"'
  Pop $HelperExit
  ${If} $HelperExit != 0
    MessageBox MB_OK|MB_ICONSTOP "Не удалось подготовить WebView2. При первой установке нужен интернет для загрузки компонента Microsoft. Установите WebView2 Evergreen Runtime и повторите запуск. Текущий Лес не заменён." /SD IDOK
    SetErrorLevel 1
    Abort
  ${EndIf}
  SetOutPath "$PLUGINSDIR\payload"
  File /r "${PAYLOAD_DIR}\*"
  SetOutPath "$PLUGINSDIR"
  File "package.ps1"
  nsExec::ExecToLog 'powershell.exe -NoProfile -NonInteractive -ExecutionPolicy Bypass -File "$PLUGINSDIR\package.ps1" -Mode Install -Source "$PLUGINSDIR\payload"'
  Pop $HelperExit
  ${If} $HelperExit != 0
    MessageBox MB_OK|MB_ICONSTOP "Не удалось установить LES RAG. Закройте Лес и повторите установку. Подробности доступны в журнале этого окна." /SD IDOK
    SetErrorLevel 1
    Abort
  ${EndIf}
  SetOutPath "$INSTDIR"
  WriteUninstaller "$INSTDIR\Uninstall.exe"
  Delete "$SMPROGRAMS\ЛЕС Light\ЛЕС Light.lnk"
  Delete "$SMPROGRAMS\ЛЕС Light\Удалить ЛЕС Light.lnk"
  RMDir "$SMPROGRAMS\ЛЕС Light"
  CreateDirectory "$SMPROGRAMS\LES RAG"
  CreateShortcut "$SMPROGRAMS\LES RAG\LES RAG.lnk" "$INSTDIR\les-light.exe"
  CreateShortcut "$SMPROGRAMS\LES RAG\Удалить LES RAG.lnk" "$INSTDIR\Uninstall.exe"
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\me.ovc.les-light" "DisplayName" "LES RAG"
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\me.ovc.les-light" "DisplayVersion" "${PRODUCT_VERSION}"
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\me.ovc.les-light" "UninstallString" '"$INSTDIR\Uninstall.exe"'
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\me.ovc.les-light" "InstallLocation" "$INSTDIR"
  WriteRegDWORD HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\me.ovc.les-light" "NoModify" 1
  WriteRegDWORD HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\me.ovc.les-light" "NoRepair" 1
SectionEnd

Function un.DataPage
  nsDialogs::Create 1018
  Pop $0
  ${NSD_CreateLabel} 0 0 100% 36u "Документы, почта и настройки сохранятся. После повторной установки вы сможете продолжить работу."
  Pop $0
  ${NSD_CreateCheckbox} 0 48u 100% 30u "Также безвозвратно удалить все мои данные LES RAG"
  Pop $RemoveDataCheckbox
  nsDialogs::Show
FunctionEnd

Function un.DataPageLeave
  ${NSD_GetState} $RemoveDataCheckbox $RemoveData
FunctionEnd

Section "Uninstall"
  InitPluginsDir
  SetOutPath "$PLUGINSDIR"
  File "package.ps1"
  StrCpy $0 ""
  ${If} $RemoveData == ${BST_CHECKED}
    StrCpy $0 "-RemoveUserData"
  ${EndIf}
  nsExec::ExecToLog 'powershell.exe -NoProfile -NonInteractive -ExecutionPolicy Bypass -File "$PLUGINSDIR\package.ps1" -Mode Remove $0'
  Pop $HelperExit
  ${If} $HelperExit != 0
    MessageBox MB_OK|MB_ICONSTOP "Не удалось удалить LES RAG. Закройте приложение и повторите удаление. Подробности доступны в журнале этого окна." /SD IDOK
    SetErrorLevel 1
    Abort
  ${EndIf}
  Delete "$SMPROGRAMS\LES RAG\LES RAG.lnk"
  Delete "$SMPROGRAMS\LES RAG\Удалить LES RAG.lnk"
  RMDir "$SMPROGRAMS\LES RAG"
  DeleteRegKey HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\me.ovc.les-light"
SectionEnd
