@echo off
setlocal
if exist "%~dp0release\System Bio Mon\System Bio Mon.exe" (
  start "" "%~dp0release\System Bio Mon\System Bio Mon.exe"
) else (
  start "" "%~dp0app\src-tauri\target\release\system-bio-mon.exe"
)
