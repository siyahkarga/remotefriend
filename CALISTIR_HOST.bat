@echo off
REM RemoteFriend Host baslat (ekrani paylasan taraf)
cd /d "%~dp0"
if not defined REMOTE_FRIEND_PASS set REMOTE_FRIEND_PASS=1234
echo Sifre: %REMOTE_FRIEND_PASS%  (degistirmek icin: set REMOTE_FRIEND_PASS=yenisifre)
echo IP adresin:
ipconfig | findstr /i "IPv4"
echo.
echo Client su sekilde baglanacak: .\remote-friend-client.exe SENIN_IP:33200 %REMOTE_FRIEND_PASS%
echo.
.\target\release\remote-friend-host.exe
pause
