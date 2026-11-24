@echo off
setlocal
title System Bio Mon
pushd "%~dp0daemon" || exit /b 1

where node.exe >nul 2>nul
if errorlevel 1 (
  echo Install Node.js 18 or later to run the local browser dashboard.
  pause
  exit /b 1
)

if not exist "node_modules\ws\package.json" (
  echo Installing local dashboard dependencies...
  call npm.cmd ci --no-fund --no-audit
  if errorlevel 1 (
    pause
    exit /b 1
  )
)

start "System Bio Mon - Local Monitor" node.exe index.js
timeout /t 2 /nobreak >nul
start "" "http://127.0.0.1:4477/"
echo Close the System Bio Mon - Local Monitor window to stop the dashboard.
popd
endlocal
