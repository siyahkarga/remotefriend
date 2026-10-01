@echo off
REM RemoteFriend Host - cift tikla, bitti. Ayar gerekmez.
cd /d "%~dp0"
if not defined REMOTE_FRIEND_PASS set REMOTE_FRIEND_PASS=1234
if not defined RF_RV_SERVER set RF_RV_SERVER=169.58.37.61:33202
if not defined RF_RV_FP set RF_RV_FP=f827d57c794820e2a1dbe419e4dabe495d922cc71b4e426ede14564f2728dade
echo Sifre: %REMOTE_FRIEND_PASS%
echo Sunucu: %RF_RV_SERVER%
echo.
echo Ekrandaki 9 haneli ID'yi karsi tarafa ver.
echo Baglanti istegi gelince bu pencerede E'ye bas.
echo.
.\remote-friend-host.exe
pause
