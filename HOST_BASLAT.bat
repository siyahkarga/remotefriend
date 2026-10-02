@echo off
REM RemoteFriend Host - hazir binary baslatma
cd /d "%~dp0"
if defined REMOTE_FRIEND_PASS (
  echo REMOTE_FRIEND_PASS ayarlandi; guvenlik icin ekrana yazilmiyor.
) else (
  echo Host rastgele sifre uretecek; terminaldeki sifreyi istemciye gir.
)
if not defined RF_FPS set RF_FPS=30
if not defined RF_MAX_WIDTH set RF_MAX_WIDTH=1600
if not defined RF_BITRATE_BPS set RF_BITRATE_BPS=6000000
if defined RF_RV_SERVER (
  echo Internet sunucusu: %RF_RV_SERVER%
  if not defined RF_RV_FP echo UYARI: RF_RV_FP yok; guvenli TLS baglantisi kurulmaz.
) else (
  echo Internet modu kapali; sadece LAN. Kendi VPS'in icin RF_RV_SERVER ve RF_RV_FP ayarla.
)
echo.
echo Ekrandaki 9 haneli ID'yi ve uretilen sifreyi karsi tarafa ver.
echo Baglanti istegi gelince bu pencerede E'ye bas.
echo.
.\remote-friend-host.exe
pause
