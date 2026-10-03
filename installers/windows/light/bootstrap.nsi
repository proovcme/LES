Unicode true
!include "MUI2.nsh"
!include "LogicLib.nsh"
!ifndef OUTPUT_FILE
!define OUTPUT_FILE "LES-RAG-Web-Setup.exe"
!endif
Name "LES RAG"
Caption "LES RAG — установка из интернета"
OutFile "${OUTPUT_FILE}"
RequestExecutionLevel user
SetCompressor /SOLID lzma
Var HelperExit
Var InstallArgs
!insertmacro MUI_PAGE_WELCOME
!insertmacro MUI_PAGE_INSTFILES
!insertmacro MUI_PAGE_FINISH
!insertmacro MUI_LANGUAGE "Russian"
Section "Скачать и установить LES RAG"
  InitPluginsDir
  SetOutPath "$PLUGINSDIR"
  File "install-les.ps1"
  StrCpy $InstallArgs ""
  ${If} ${Silent}
    StrCpy $InstallArgs "-Silent"
  ${EndIf}
  DetailPrint "Скачивание проверенного пакета LES RAG с GitHub..."
  nsExec::ExecToLog 'powershell.exe -NoProfile -NonInteractive -ExecutionPolicy Bypass -File "$PLUGINSDIR\install-les.ps1" $InstallArgs'
  Pop $HelperExit
  ${If} $HelperExit != 0
    MessageBox MB_OK|MB_ICONSTOP "Не удалось скачать или установить LES RAG. Проверьте интернет и наличие опубликованного выпуска. Повторите запуск или используйте полный установщик." /SD IDOK
    SetErrorLevel 1
    Abort
  ${EndIf}
SectionEnd
