@echo off
chcp 65001 >nul
REM RemoteFriend tek tikla kurulum + baslatma (Windows 64-bit).
REM Yeni surum varsa programlari gunceller, sunucu ayarini bir kez sorar
REM (settings.local.bat icinde kalir), guvenlik duvarini acar, host'u baslatir.
cd /d "%~dp0"
set BASE=https://github.com/siyahkarga/remotefriend/releases/latest/download

set LATEST=
for /f "usebackq delims=" %%v in (`powershell -NoProfile -Command "try { (Invoke-RestMethod -TimeoutSec 8 https://api.github.com/repos/siyahkarga/remotefriend/releases/latest).tag_name } catch { '' }"`) do set LATEST=%%v
set CURRENT=
if exist .rf-version set /p CURRENT=<.rf-version

set NEED=0
if not exist remote-friend-host.exe set NEED=1
if not exist remote-friend-client.exe set NEED=1
if defined LATEST if not "%LATEST%"=="%CURRENT%" set NEED=1
if "%NEED%"=="1" (
  echo Indiriliyor %LATEST% ...
  curl -fsSL %BASE%/remote-friend-host.exe -o remote-friend-host.new && move /y remote-friend-host.new remote-friend-host.exe >nul
  curl -fsSL %BASE%/remote-friend-client.exe -o remote-friend-client.new && move /y remote-friend-client.new remote-friend-client.exe >nul
  if defined LATEST (echo %LATEST%)> .rf-version
)
if not exist remote-friend-host.exe (
  echo INDIRME HATASI: internet baglantisini kontrol et.
  pause
  exit /b 1
)

if exist settings.local.bat call settings.local.bat

if not defined RF_RV_SERVER (
  echo.
  echo Internetten ^(telefondan^) baglanmak icin VPS adresini yaz; sadece yerel ag icin bos birak.
  set /p RF_RV_SERVER=Sunucu [ornek 1.2.3.4:33202, bos = yerel ag]:
)

REM Ayarlari kaydet (bir kez sorar)
(
  echo @echo off
  echo REM Otomatik olusturuldu; tekrar sormasi icin bu dosyayi sil.
  if defined RF_RV_SERVER echo set RF_RV_SERVER=%RF_RV_SERVER%
  if defined RF_WEB_URL echo set RF_WEB_URL=%RF_WEB_URL%
  if defined RF_RV_FP echo set RF_RV_FP=%RF_RV_FP%
  if defined REMOTE_FRIEND_PASS echo set REMOTE_FRIEND_PASS=%REMOTE_FRIEND_PASS%
)> settings.local.bat

REM Guvenlik duvari (yonetici gerekir, degilse sessizce atlanir)
netsh advfirewall firewall add rule name="RemoteFriend" dir=in action=allow protocol=TCP localport=33200,33201 >nul 2>nul

echo.
.\remote-friend-host.exe
pause
