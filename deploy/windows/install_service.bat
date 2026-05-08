@echo off
REM Install dnp3_master_io_v3 as a Windows service via NSSM.
REM Run this from an elevated cmd.exe in the same folder as the .exe.

setlocal
set SVC=dnp3_master_io_v3
set HERE=%~dp0
set EXE=%HERE%dnp3_master_io_v3.exe
set LOGDIR=%HERE%logs

where nssm >nul 2>nul
if errorlevel 1 (
  echo [error] nssm.exe not found on PATH.
  echo         Download from https://nssm.cc/download and place nssm.exe
  echo         in this folder or anywhere on PATH, then re-run.
  exit /b 1
)

if not exist "%EXE%" (
  echo [error] %EXE% not found.
  exit /b 1
)

if not exist "%HERE%rtus.toml" (
  echo [warn ] rtus.toml not present — service will fail to start until
  echo         you copy rtus.toml.example to rtus.toml and edit it.
)

if not exist "%LOGDIR%" mkdir "%LOGDIR%"

nssm install %SVC% "%EXE%"
nssm set %SVC% AppDirectory "%HERE%"
nssm set %SVC% DisplayName "DNP3 Master IO v3"
nssm set %SVC% Description "DNP3 fleet monitor and REST/UI on :9002"
nssm set %SVC% Start SERVICE_AUTO_START
nssm set %SVC% AppStdout "%LOGDIR%\stdout.log"
nssm set %SVC% AppStderr "%LOGDIR%\stderr.log"
nssm set %SVC% AppRotateFiles 1
nssm set %SVC% AppRotateOnline 1
nssm set %SVC% AppRotateBytes 10485760
nssm set %SVC% AppEnvironmentExtra MASTER_LOG=info

echo.
echo Installed. Start with:  net start %SVC%
echo Stop with:              net stop  %SVC%
echo Logs:                   %LOGDIR%
endlocal
