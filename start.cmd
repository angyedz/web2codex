@echo off
setlocal
set "CODEX_WINDOWS_REGISTERED_CORE=0"
set "WEB2CODEX_PACKAGE="
for /f "delims=" %%D in ('dir /b /ad /o:n "%~dp0dist\web2codex-prealpha-windows-x64-*" 2^>nul') do (
    call :is_complete_package "%~dp0dist\%%D"
    if not errorlevel 1 set "WEB2CODEX_PACKAGE=%~dp0dist\%%D"
)
if not defined WEB2CODEX_PACKAGE set "WEB2CODEX_PACKAGE=%~dp0dist\web2codex-prealpha-windows-x64"
call :is_complete_package "%WEB2CODEX_PACKAGE%"
if errorlevel 1 (
    echo A complete Web2Codex package was not found under "%~dp0dist".
    exit /b 1
)
call "%WEB2CODEX_PACKAGE%\web2codex.cmd" %*
exit /b %errorlevel%

:is_complete_package
if not exist "%~1\codex-package.json" exit /b 1
if not exist "%~1\web2codex.cmd" exit /b 1
if not exist "%~1\bin\web2codex.exe" exit /b 1
if not exist "%~1\bin\codex-code-mode-host.exe" exit /b 1
if not exist "%~1\bin\codex-exec-server.exe" exit /b 1
if not exist "%~1\codex-path\rg.exe" exit /b 1
if not exist "%~1\codex-resources\codex-web-browser.exe" exit /b 1
if not exist "%~1\codex-resources\codex-command-runner.exe" exit /b 1
if not exist "%~1\codex-resources\codex-windows-sandbox-setup.exe" exit /b 1
exit /b 0
