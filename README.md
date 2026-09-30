# RemoteFriend — AnyDesk benzeri, Rust, P2P (v0.2.0)

Durum: **LAN'da çalışıyor.** İnternet P2P (hole-punch/relay) sonraki faz.

## Ne var? (v0.2.0)
- `host`: H264 ekran yayını (openh264, 1080p, ~15fps, ~1-4 Mbps) + tam kontrol (mouse sol/sağ, scroll, tüm klavye: harf/ok/F1-F12/shift/ctrl/alt) + dosya alma
- `client`: terminal GEREKTİRMEZ — açılışta adres+şifre ekranı; canlı görüntü + tıklama + yazı + scroll + "Dosya Gönder" butonu
- ÖNEMLİ: v0.2.0 protokolü değiştirdi — **host ve client ikisi de v0.2.0 olmalı**

## Hızlı test (aynı ağ, terminalsiz)
1. Host PC: `remote-friend-host.exe` çalıştır (şifre: 1234)
2. Client PC: `remote-friend-client` çalıştır → adres yaz (`192.168.178.31:33200`) → Bağlan

Headless testler (GUI'siz):
```
cargo run -p remote-friend-client --example headless -- 127.0.0.1:33200 1234
cargo run -p remote-friend-client --example inputtest -- 127.0.0.1:33200 1234
```

## Bu PC (Linux Wayland) notu
xcap GNOME-Wayland'da bazen portal hatası verir (tekrar dene / Xorg ile giriş yap). **Windows host sorunsuz**, bu Linux = client izler.

## Windows kurulumu (derlemesiz)
Releases'ten indir → çıkar → `HOST_BASLAT.bat` çift tık. Bkz: `WINDOWS_KURULUM.md`.

## Sonraki faz (yapılmadı)
- QUIC (quinn) + donanım encode (NVENC/QSV/AMF)
- İnternet: sinyal sunucusu + hole-punch + TURN relay (AnyDesk-ID benzeri)
- Ses, pano, çoklu monitör, servis olarak çalıştırma
