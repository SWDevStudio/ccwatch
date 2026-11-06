@echo off
rem ccwatch hook fast path: node starts slowly, so run it only when ccwatch left a plan request.
rem ASCII only: cmd.exe reads .cmd files in the OEM code page.
set "CCW_DIR=%CCWATCH_DIR%"
if "%CCW_DIR%"=="" set "CCW_DIR=%USERPROFILE%\.ccwatch"
if not exist "%CCW_DIR%\ask\*.json" exit /b 0
node "%~dp0ccwatch-hook.js"
