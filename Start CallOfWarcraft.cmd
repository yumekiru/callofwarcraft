@echo off
where pwsh.exe >nul 2>nul
if errorlevel 1 (
    echo Install PowerShell 7 and ensure pwsh.exe is on PATH.
    pause
    exit /b 1
)
pwsh.exe -NoLogo -NoProfile -ExecutionPolicy Bypass -File "%~dp0scripts\Start-Game.ps1"
if errorlevel 1 pause
