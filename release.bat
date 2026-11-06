@echo off
setlocal
chcp 65001 >nul
cd /d "%~dp0"

where python >nul 2>nul
if %errorlevel% equ 0 (
    set "PY_CMD=python"
    goto :RUN
)

where py >nul 2>nul
if %errorlevel% equ 0 (
    set "PY_CMD=py"
    goto :RUN
)

echo [ERROR] Python not found in PATH. Please install Python.
pause
exit /b 1

:RUN
"%PY_CMD%" "%~dp0release.py" %*
set "EXIT_CODE=%errorlevel%"

if "%~1"=="" (
    pause
) else if %EXIT_CODE% neq 0 (
    pause
)

exit /b %EXIT_CODE%
