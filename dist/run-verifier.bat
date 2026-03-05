@echo off
setlocal

set "SCRIPT_DIR=%~dp0"
set "BIN_PATH=%SCRIPT_DIR%rust-native-verifier.exe"

if not exist "%BIN_PATH%" (
  echo Error: %BIN_PATH% not found.
  echo Make sure this batch file is next to rust-native-verifier.exe.
  pause
  exit /b 1
)

if "%~1"=="" (
  "%BIN_PATH%" --node-type auto --pause-on-exit
) else (
  "%BIN_PATH%" %*
)
