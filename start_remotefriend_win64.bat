@echo off
REM Terminal host (alternative). Most people should run RemoteFriend-Setup.exe instead.
chcp 65001 >nul
REM RemoteFriend one-click install + start (Windows 64-bit).
REM Updates the programs when a new release is out, asks for the server once
REM (saved in settings.local.bat), opens the firewall and starts the host.
cd /d "%~dp0"
set BASE=https://github.com/siyahkarga/remotefriend/releases/latest/download

set LATEST=
for /f "usebackq delims=" %%v in (`powershell -NoProfile -Command "try { (Invoke-RestMethod -TimeoutSec 8 https://api.github.com/repos/siyahkarga/remotefriend/releases/latest).tag_name } catch { '' }"`) do set LATEST=%%v
set CURRENT=
if exist .rf-version set /p CURRENT=<.rf-version

set NEED=0
if not exist remote-friend-host.exe set NEED=1
if defined LATEST if not "%LATEST%"=="%CURRENT%" set NEED=1
if "%NEED%"=="1" (
  echo Downloading %LATEST% ...
  curl -fsSL %BASE%/remote-friend-host.exe -o remote-friend-host.new && move /y remote-friend-host.new remote-friend-host.exe >nul
  if defined LATEST (echo %LATEST%)> .rf-version
)
if not exist remote-friend-host.exe (
  echo DOWNLOAD ERROR: check your internet connection.
  pause
  exit /b 1
)

if exist settings.local.bat call settings.local.bat

if not defined RF_RV_SERVER (
  echo.
  echo To connect over the internet ^(e.g. from your phone^), enter the VPS address; leave empty for local network only.
  set /p RF_RV_SERVER=Server [e.g. 1.2.3.4:33202, empty = local network]:
)

REM Save settings (asked only once)
(
  echo @echo off
  echo REM Generated automatically; delete this file to be asked again.
  if defined RF_RV_SERVER echo set RF_RV_SERVER=%RF_RV_SERVER%
  if defined RF_WEB_URL echo set RF_WEB_URL=%RF_WEB_URL%
  if defined RF_RV_FP echo set RF_RV_FP=%RF_RV_FP%
  if defined REMOTE_FRIEND_PASS echo set REMOTE_FRIEND_PASS=%REMOTE_FRIEND_PASS%
)> settings.local.bat

REM Firewall rule (needs administrator rights; silently skipped otherwise)
netsh advfirewall firewall add rule name="RemoteFriend" dir=in action=allow protocol=TCP localport=33200,33201 >nul 2>nul

echo.
.\remote-friend-host.exe
pause
