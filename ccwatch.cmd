@echo off
rem Runs the Rust build of ccwatch: build it first with cargo build --release
if exist "%~dp0target\release\ccwatch.exe" goto run
echo ccwatch.exe not found: build it with cargo build --release
exit /b 1
:run
"%~dp0target\release\ccwatch.exe" %*
