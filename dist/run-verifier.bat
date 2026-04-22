@echo off
setlocal

set "SCRIPT_DIR=%~dp0"
set "BIN_PATH=%SCRIPT_DIR%genesis-proof.exe"

if not exist "%BIN_PATH%" (
  echo Error: %BIN_PATH% not found.
  echo Make sure this batch file is next to genesis-proof.exe.
  pause
  exit /b 1
)

if "%~1"=="" (
  "%BIN_PATH%" --node-type auto --pause-on-exit
) else (
  "%BIN_PATH%" %*
)
