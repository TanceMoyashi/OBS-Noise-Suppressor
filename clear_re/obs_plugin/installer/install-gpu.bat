@echo off
rem ===========================================================================
rem  OBS Noise Suppressor - GPU (DirectML) add-on installer
rem
rem  This launcher is intentionally ASCII-only (see install.bat for why).
rem  All logic and the Japanese messages live in install-gpu.ps1.
rem
rem  Install the main package first (OBS-Noise-Suppressor-x.y.z-win64.zip).
rem
rem  Usage:
rem    install-gpu.bat                       install to the default location
rem    install-gpu.bat -Obs "D:\obs-studio"  portable OBS installation
rem
rem  Set OBSNS_NOPAUSE=1 to skip the final "Press any key".
rem ===========================================================================
setlocal
set "RC=0"
powershell.exe -NoProfile -ExecutionPolicy Bypass -File "%~dp0install-gpu.ps1" %*
set "RC=%ERRORLEVEL%"
if defined OBSNS_NOPAUSE goto done
echo.
pause
:done
exit /b %RC%
