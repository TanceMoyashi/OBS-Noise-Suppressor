@echo off
rem ===========================================================================
rem  OBS Noise Suppressor - installer (double-click this file)
rem
rem  This launcher is intentionally ASCII-only.
rem
rem  cmd.exe can lose track of its position in a batch file that contains
rem  multi-byte characters (verified: Japanese lines get split and parts of
rem  comments are executed as commands, especially in scripts with many
rem  labels and gotos). PowerShell reads the UTF-8 (BOM) script correctly,
rem  so all logic and the Japanese messages live in install.ps1.
rem
rem  No registry writes, no admin rights needed for the default location.
rem
rem  Usage:
rem    install.bat                       install to the default location
rem    install.bat -Obs "D:\obs-studio"  also install into a portable OBS
rem    install.bat -NoDetect             skip OBS detection
rem    install.bat -Force                run even while OBS is running
rem
rem  Set OBSNS_NOPAUSE=1 to skip the final "Press any key".
rem ===========================================================================
setlocal
set "RC=0"
powershell.exe -NoProfile -ExecutionPolicy Bypass -File "%~dp0install.ps1" %*
set "RC=%ERRORLEVEL%"
if defined OBSNS_NOPAUSE goto done
echo.
pause
:done
exit /b %RC%
