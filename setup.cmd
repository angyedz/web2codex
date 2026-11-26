@echo off
setlocal
set "WEB2CODEX_SETUP_ON_START=1"
call "%~dp0start.cmd" %*
exit /b %errorlevel%
