# RemoteFriend — AnyDesk benzeri, Rust, P2P (LAN MVP hazır)

Durum: **LAN'da çalışıyor.** İnternet P2P (hole-punch/relay) sonraki faz.

## Ne var?
- `host`: ekranı paylaşan (xcap + JPEG 10fps, max 1600px) + mouse/klavye uygular (enigo) + dosya alır (`/tmp/rf_*` veya `%TEMP%`)
- `client`: eframe ile canlı görüntü + tıkla/sürükle mouse gönder + yazı/enter/space klavye gönder + "Dosya Gönder" butonu
- `common`: bincode protokol (Handshake/Accept/Video/Input/File)

## Hızlı test (aynı ağ)
Host PC:
```
REMOTE_FRIEND_PASS=1234 ./target/release/remote-friend-host
# dinler: 0.0.0.0:33200
```
Client PC:
```
./target/release/remote-friend-client 192.168.178.52:33200 1234
```

Headless protokol testi (GUI'siz):
```
cargo run -p remote-friend-client --example headless -- 127.0.0.1:33200 1234
```

## Bu PC (Linux Wayland) notu
xcap GNOME-Wayland'da `ZwlrScreencopy not found` verir. 2 seçenek:
1. **Windows laptop = host** (önerilen, sorunsuz), bu Linux = client izler.
2. Bu Linux host olacaksa: çıkış yap → giriş ekranında "GNOME on Xorg" seç → host çalışır.

## Windows laptop kurulumu
Bkz: `WINDOWS_KURULUM.md` — 5 dk: Rust kur + klasörü kopyala + `cargo build --release` + firewall izin + çalıştır.

## Sonraki faz (yapılmadı)
- QUIC (quinn) + H264 (openh264) + donanım encode
- İnternet: iroh/libp2p hole-punch + rendezvous + TURN relay
- Ses, pano, çoklu monitör, servis olarak çalıştırma
