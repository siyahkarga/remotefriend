@echo off
REM RemoteFriend one-click setup + start (Windows 64-bit).
REM Downloads missing exes from the latest release, asks server settings once,
REM saves them to settings.local.bat (stays on your machine), opens firewall, runs host.
cd /d "%~dp0"
set BASE=https://github.com/siyahkarga/remotefriend/releases/latest/download

if not exist remote-friend-host.exe (
  echo Downloading host...
  curl -sL %BASE%/remote-friend-host.exe -o remote-friend-host.exe
)
if not exist remote-friend-client.exe (
  echo Downloading client...
  curl -sL %BASE%/remote-friend-client.exe -o remote-friend-client.exe
)
if not exist remote-friend-host.exe (
  echo DOWNLOAD FAILED: check internet and try again.
  pause
  exit /b 1
)

if exist settings.local.bat call settings.local.bat

if not defined RF_RV_SERVER (
  echo.
  echo Leave server empty for LAN-only mode.
  set /p RF_RV_SERVER=Server [example 1.2.3.4:33202, empty=LAN]:
)
if defined RF_RV_SERVER (
  REM First run asks once in the app and remembers; RF_RV_FP override stays optional.
  if not defined RF_RV_FP (
    echo First run will ask once to trust the server, then remembers.
  )
)

if not defined REMOTE_FRIEND_PASS (
  echo No password set: the program generates a new one on every start.
)

REM Save settings (asks only once)
(
  echo @echo off
  echo REM Auto-generated, delete to ask again.
  if defined RF_RV_SERVER echo set RF_RV_SERVER=%RF_RV_SERVER%
  if defined RF_RV_FP echo set RF_RV_FP=%RF_RV_FP%
  if defined REMOTE_FRIEND_PASS echo set REMOTE_FRIEND_PASS=%REMOTE_FRIEND_PASS%
)> settings.local.bat

REM Firewall (needs admin, ignored otherwise)
netsh advfirewall firewall add rule name="RemoteFriend" dir=in action=allow protocol=TCP localport=33200,33201 >nul 2>nul

echo.
echo Starting host...
.\remote-friend-host.exe
pause
