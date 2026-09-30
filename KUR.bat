@echo off
REM RemoteFriend Windows tek-tik kurulum
cd /d "%~dp0"
echo === RemoteFriend kurulum ===

where cargo >nul 2>nul
if %errorlevel% neq 0 (
  echo Rust yok, kuruluyor...
  echo Lutfen https://rustup.rs adresinden rustup-init.exe indirip kurun,
  echo sonra bu dosyayi tekrar calistirin.
  echo.
  pause
  start https://rustup.rs
  exit /b 1
)

echo Build aliniyor (ilk sefer 3-8 dk surebilir)...
cargo build --release
if %errorlevel% neq 0 (
  echo BUILD HATASI - Visual Studio Build Tools gerekli olabilir:
  echo https://visualstudio.microsoft.com/downloads/ - C++ build tools
  pause
  exit /b 1
)

echo Firewall izni ekleniyor (admin gerekli)...
netsh advfirewall firewall add rule name="RemoteFriend" dir=in action=allow protocol=TCP localport=33200 >nul 2>nul

echo.
echo === HAZIR ===
echo Host baslatmak icin: CALISTIR_HOST.bat dosyasina cift tikla
echo.
pause
