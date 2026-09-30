@echo off
REM Hazir exe ile host baslat (derleme gerekmez)
cd /d "%~dp0"
if not defined REMOTE_FRIEND_PASS set REMOTE_FRIEND_PASS=1234
echo Sifre: %REMOTE_FRIEND_PASS%
echo IP adresin:
ipconfig | findstr /i "IPv4"
echo.
.\remote-friend-host.exe
pause
