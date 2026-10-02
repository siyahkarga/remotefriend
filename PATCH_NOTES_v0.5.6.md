# v0.5.6 Hotfix Değişiklikleri

## Performans

- Hedef 15 FPS → yapılandırılabilir 30 FPS.
- `işlem süresi + 66 ms` pacing hatası giderildi.
- Tek global H.264 ve tek global JPEG yayın hattı.
- Küçük broadcast kuyrukları ve lag durumunda eski kare atlama.
- İstemcide ilk kare deadlock'u giderildi.
- İstemcide tam RGBA kare clone'u ve gereksiz texture upload kaldırıldı.
- Tarayıcı H.264/JPEG decode backpressure eklendi.
- Monitör enumeration cache, kalıcı input worker ve dosya streaming eklendi.

## Güvenlik

- Protokol `2`, transport nesli `4`.
- İnternet web parolası hostta doğrulanıyor.
- TLS handshake imza doğrulaması düzeltildi.
- CSPRNG host ID, 256 bit kalıcı host secret ve 128 bit dial-back token.
- Rastgele varsayılan oturum şifresi.
- Origin kontrolü, CSP ve diğer HTTP güvenlik başlıkları.
- Bounded kanallar, oturum limitleri, timeoutlar ve boyut sınırları.
- Güvenli gelen dosya dizini/adı/chunk sırası/atomik tamamlanma.
- Parola localStorage'dan kaldırıldı.
- VPS relay'in E2E olmadığı dokümante edildi.

## Uyumsuz değişiklik

Eski binary'lerle protokol uyumlu değildir. Host, client ve rendezvous birlikte güncellenmelidir.

## Doğrulama durumu

Bu kaynak pakette statik tutarlılık, TOML/JSON ve tarayıcı JavaScript sözdizimi kontrolleri yapılmıştır. Paketi hazırlayan ortamda Rust toolchain bulunmadığı için nihai `cargo build` çalıştırılamamıştır; gerçek kullanım öncesi CI veya yerel makinede release build/test zorunludur.
