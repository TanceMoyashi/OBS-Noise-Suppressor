@echo off
rem ===========================================================================
rem  DPDFNet Voice - VST3 uninstaller
rem
rem  ASCII-only launcher; messages are in uninstall.ps1.
rem
rem  Usage:
rem    uninstall.bat                 remove from the default locations
rem    uninstall.bat -User           remove only from %APPDATA%\VST3
rem    uninstall.bat -Dest "D:\VST3" custom folder
rem
rem  Set DPDFNETV_NOPAUSE=1 to skip the final "Press any key".
rem ===========================================================================
setlocal
set "RC=0"
powershell.exe -NoProfile -ExecutionPolicy Bypass -File "%~dp0uninstall.ps1" %*
set "RC=%ERRORLEVEL%"
if defined DPDFNETV_NOPAUSE goto done
echo.
pause
:done
exit /b %RC%
