@echo off
rem ===========================================================================
rem  DPDFNet Voice - VST3 installer
rem
rem  This launcher is intentionally ASCII-only (cmd.exe reads .bat in the
rem  console code page, so Japanese would break on some machines).
rem  All logic and the Japanese messages live in install.ps1.
rem
rem  Usage:
rem    install.bat                          install to C:\Program Files\Common Files\VST3
rem    install.bat -User                    install to %APPDATA%\VST3 (no admin)
rem    install.bat -Dest "D:\VST3"          custom folder
rem
rem  Set DPDFNETV_NOPAUSE=1 to skip the final "Press any key".
rem ===========================================================================
setlocal
set "RC=0"
powershell.exe -NoProfile -ExecutionPolicy Bypass -File "%~dp0install.ps1" %*
set "RC=%ERRORLEVEL%"
if defined DPDFNETV_NOPAUSE goto done
echo.
pause
:done
exit /b %RC%
