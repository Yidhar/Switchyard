@echo off
setlocal enabledelayedexpansion

:: Check CARGO_TARGET_DIR environment variable
if defined CARGO_TARGET_DIR (
    set "TD_VAR=!CARGO_TARGET_DIR!"
    if "!TD_VAR:~0,7!"=="target-" (
        set "CARGO_TARGET_DIR=target\!TD_VAR!"
    )
)

:: Rebuild arguments, rewriting --target-dir target-xxx to --target-dir target/target-xxx
set "NEW_ARGS="
:loop
if "%~1"=="" goto end_loop
set "arg=%~1"

if "%arg%"=="--target-dir" (
    set "next_arg=%~2"
    if "!next_arg:~0,7!"=="target-" (
        set "NEW_ARGS=!NEW_ARGS! --target-dir target\!next_arg!"
        shift
    ) else (
        set "NEW_ARGS=!NEW_ARGS! --target-dir "!next_arg!""
        shift
    )
) else (
    set "NEW_ARGS=!NEW_ARGS! "%~1""
)
shift
goto loop

:end_loop
:: Run the real cargo.exe from system PATH.
for /f "tokens=*" %%i in ('where cargo.exe') do (
    set "cargo_path=%%i"
    echo !cargo_path! | findstr /i /c:"E:\\Switchyard" >nul
    if errorlevel 1 (
        "!cargo_path!" %NEW_ARGS%
        exit /b %errorlevel%
    )
)

cargo.exe %NEW_ARGS%
