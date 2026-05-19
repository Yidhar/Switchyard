@echo off
setlocal
set "SWITCHYARD_ROOT=%~dp0"
set "SWITCHYARD_BIN=%SWITCHYARD_ROOT%target\debug\switchyard.exe"
if exist "%SWITCHYARD_BIN%" goto run
set "SWITCHYARD_BIN=%SWITCHYARD_ROOT%target\release\switchyard.exe"
if exist "%SWITCHYARD_BIN%" goto run
echo switchyard binary not found under "%SWITCHYARD_ROOT%target\debug" or "%SWITCHYARD_ROOT%target\release". 1>&2
exit /b 1
:run
"%SWITCHYARD_BIN%" %*
exit /b %ERRORLEVEL%
