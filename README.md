# RemoteFriend v0.4.0 — tarayıcıdan bağlan, kurulum yok

## En kolay yol (önerilen)
1. Host PC'de `remote-friend-host.exe` çalıştır. Ekranda şunu görürsün:
   ```
   Tarayıcı ile bağlan: http://192.168.X.X:33201  (aynı ağdan)
   ```
2. Aynı ağdaki HERHANGİ cihazdan (PC, telefon, tablet) tarayıcıda o adresi aç.
   Chrome/Edge önerilir (H264 WebCodecs gerekir).
3. Şifreyi gir (varsayılan `1234`) → Bağlan → **host terminalinde E'ye bas (onay şart!)**.
4. Bitti: canlı görüntü + mouse + klavye + scroll + dosya gönderme.

Gereken portlar (host PC firewall): TCP `33200` (native client) + TCP `33201` (tarayıcı).

## Native client (alternatif)
`remote-friend-client` çalıştır → adres yaz → Bağlan. LAN'daki hostlar otomatik listelenir,
son bağlantılar hatırlanır. ⛔ Kes ile çıkılır.

## Önemli
- Her bağlantı host ONAYI ister (E/H, 30 sn, varsayılan ret). Onaysız yayın başlamaz.
- Bir host'a aynı anda birden fazla izleyici bağlanabilir.
- v0.4.0 protokolü değiştirdi: iki taraf da v0.4.0 olmalı.

## Ortam değişkenleri (host)
- `REMOTE_FRIEND_PASS` — şifre (varsayılan 1234)
- `REMOTE_FRIEND_AUTO_ACCEPT=1` — onaysız kabul (sadece güvenli/test ortamı!)
- `RF_HTTP_PORT` — web portu (varsayılan 33201)
- `REMOTE_FRIEND_DIR` — gelen dosyalar (varsayılan /tmp ya da %TEMP%)

## Sonraki faz
- İnternet (farklı ağ): sinyal sunucusu + AnyDesk-tarzı kod + hole-punch
- QUIC şifreli transport, pano paylaşımı, ses
