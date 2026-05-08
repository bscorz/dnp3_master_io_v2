@echo off
REM Stop and remove the dnp3_master_io_v3 service.
REM Run from an elevated cmd.exe.

setlocal
set SVC=dnp3_master_io_v3

where nssm >nul 2>nul
if errorlevel 1 (
  echo [error] nssm.exe not found on PATH.
  exit /b 1
)

nssm stop   %SVC%
nssm remove %SVC% confirm

echo.
echo Removed.
endlocal
