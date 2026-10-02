@echo off
REM RemoteFriend tek-tik kurulum + baslat (Windows).
REM Exe yoksa Releases'tan indirir, sunucu ayarlarini bir kez sorar,
REM AYARLARIM.bat dosyasina kaydeder (bu dosya sende kalir, repoya gitmez).
cd /d "%~dp0"
set VER=v0.5.6

if not exist remote-friend-host.exe (
  echo Host indiriliyor (%VER%)...
  curl -sL https://github.com/siyahkarga/remotefriend/releases/download/%VER%/remote-friend-host.exe -o remote-friend-host.exe
  if not exist remote-friend-host.exe (
    echo INDIRME BASARISIZ: interneti kontrol et.
    pause
    exit /b 1
  )
)

if exist AYARLARIM.bat call AYARLARIM.bat

if not defined RF_RV_SERVER (
  echo.
  echo Sunucu bos birakilirsa sadece LAN calisir.
  set /p RF_RV_SERVER=Sunucu [ornek 1.2.3.4:33202, bos=LAN]:
)
if defined RF_RV_SERVER (
  if not defined RF_RV_FP (
    set /p RF_RV_FP=Sertifika parmak izi:
  )
)

if not defined REMOTE_FRIEND_PASS (
  echo Sifre bos: program her acilista yeni sifre uretir.
)

REM Ayarlari Kaydet (bir dahaki sefere sormaz)
(
  echo @echo off
  echo REM Otomatik olustu, silersen tekrar sorar.
  if defined RF_RV_SERVER echo set RF_RV_SERVER=%RF_RV_SERVER%
  if defined RF_RV_FP echo set RF_RV_FP=%RF_RV_FP%
  if defined REMOTE_FRIEND_PASS echo set REMOTE_FRIEND_PASS=%REMOTE_FRIEND_PASS%
)> AYARLARIM.bat

echo.
echo Baslatiliyor...
.\remote-friend-host.exe
pause
