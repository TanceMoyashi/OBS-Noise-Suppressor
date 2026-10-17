@echo off
rem ===========================================================================
rem  OBS Noise Suppressor - uninstaller (double-click this file)
rem
rem  ASCII-only launcher. See install.bat for why (cmd.exe + multi-byte
rem  characters in a batch file is unreliable). The logic lives in
rem  uninstall.ps1, which prints proper Japanese.
rem
rem  Only this plugin is removed. OBS itself and other plugins are untouched,
rem  and the registry is not used at all.
rem
rem  Usage:
rem    uninstall.bat                       ask, then remove
rem    uninstall.bat -Yes                  remove without asking
rem    uninstall.bat -Obs "D:\obs-studio"  also clean a portable OBS
rem
rem  Set OBSNS_NOPAUSE=1 to skip the final "Press any key".
rem ===========================================================================
setlocal
set "RC=0"
powershell.exe -NoProfile -ExecutionPolicy Bypass -File "%~dp0uninstall.ps1" %*
set "RC=%ERRORLEVEL%"
if defined OBSNS_NOPAUSE goto done
echo.
pause
:done
exit /b %RC%
