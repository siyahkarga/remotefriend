@echo off
REM RemoteFriend Host baslat (ekrani paylasan taraf)
cd /d "%~dp0"
if defined REMOTE_FRIEND_PASS (
  echo REMOTE_FRIEND_PASS ayarlandi; guvenlik icin ekrana yazilmiyor.
) else (
  echo Program bu calistirma icin guclu rastgele sifre uretecek ve host ekraninda gosterecek.
)
if not defined RF_FPS set RF_FPS=30
if not defined RF_MAX_WIDTH set RF_MAX_WIDTH=1600
if not defined RF_BITRATE_BPS set RF_BITRATE_BPS=6000000
echo IP adresin:
ipconfig | findstr /i "IPv4"
echo.
echo Native client adresi: SENIN_IP:33200
echo Tarayici duz HTTP'de JPEG moduna dusebilir; akici goruntu icin native client kullan.
echo.
.\target\release\remote-friend-host.exe
pause
