# RemoteFriend v0.5.6 Hotfix

RemoteFriend; ekran görüntüsü, mouse/klavye kontrolü ve sınırlı dosya aktarımı sağlayan deneysel bir Rust uzak masaüstü projesidir. Bu hotfix, v0.5.5 kaynaklarındaki düşük FPS, gecikme birikmesi ve kritik kimlik doğrulama/TLS sorunlarını düzeltir.

## En hızlı kullanım yolu

Akıcı görüntü için **native istemciyi** kullan:

1. Host bilgisayarda `remote-friend-host` çalıştır.
2. Host ilk çalıştırmada güçlü, rastgele bir oturum şifresi üretip terminalde gösterir. Kalıcı bir şifre istenirse önceden `REMOTE_FRIEND_PASS` ayarla.
3. İstemcide LAN adresini (`192.168.x.x:33200`) veya 9 haneli internet ID'sini ve şifreyi gir.
4. Host terminalinde bağlantıyı `E` ile onayla.

Tarayıcı istemcisi de çalışır; ancak LAN IP'sindeki düz `http://...:33201` sayfası çoğu tarayıcıda güvenli bağlam sayılmaz. WebCodecs H.264 açılamazsa program JPEG moduna düşer ve video belirgin biçimde daha yavaş olur. Tarayıcı için HTTPS/WSS, aksi halde native istemci önerilir.

## Önerilen performans profilleri

Windows CMD örneği:

```bat
set RF_FPS=30
set RF_MAX_WIDTH=1600
set RF_BITRATE_BPS=6000000
remote-friend-host.exe
```

- Dengeli: `RF_FPS=30`, `RF_MAX_WIDTH=1600`, `RF_BITRATE_BPS=6000000`
- Düşük CPU/ağ: `RF_FPS=20`, `RF_MAX_WIDTH=1280`, `RF_BITRATE_BPS=4000000`
- Daha net: `RF_FPS=30`, `RF_MAX_WIDTH=1920`, `RF_BITRATE_BPS=8000000`
- JPEG fallback: `RF_JPEG_FPS=8`, `RF_JPEG_Q=62`, ayrıca `RF_MAX_WIDTH=1280`

Yazılımsal OpenH264 kodlama kullanıldığı için 1080p/30 görüntü CPU'ya bağlıdır. Aynı anda hem yüksek çözünürlük hem 60 FPS için sonraki mimari adım donanımsal NVENC/QSV/VAAPI kodlama ve tercihen WebRTC taşımasıdır.

## Portlar

- TCP `33200`: LAN native istemci; şu an düz TCP'dir. Yalnızca güvenilir LAN/VPN içinde aç.
- TCP `33201`: Host tarayıcı arayüzü; düz HTTP ise parola ve kontrol trafiği ağda şifreli değildir.
- TCP `33202`: VPS rendezvous/relay; TLS fingerprint pinning kullanır.
- UDP `33201`: LAN discovery beacon.

`33200` ve `33201` portlarını doğrudan internete açma.

## Güvenlik modeli ve önemli sınır

- Her bağlantı güçlü parola ve varsayılan olarak yerel operatör onayı ister.
- Host ID'si artık VPS'de kalıcı 256 bit cihaz sırrıyla sahiplenilir.
- TLS fingerprint doğrulamasına ek olarak sunucu handshake imzası da doğrulanır.
- WebSocket aynı-origin kontrolü, oturum/kuyruk/mesaj/dosya limitleri ve güvenli dosya adlandırması uygulanır.
- Parola tarayıcının `localStorage` alanına kaydedilmez.

**VPS relay uçtan uca şifreli değildir.** TLS VPS'de sonlanır; VPS yöneticisi görüntü, input, parola ve dosya içeriğini teknik olarak görebilir. İnternet yolunu yalnızca güvendiğin VPS üzerinde kullan. Gerçek E2E için istemci ile host arasında ayrıca Noise/PAKE benzeri bir katman veya WebRTC DTLS-SRTP gerekir.

Ayrıntılar: `SECURITY.md`, `PERFORMANCE.md`, `PATCH_NOTES_v0.5.6.md`.

## Ortam değişkenleri

### Host

- `REMOTE_FRIEND_PASS`: oturum şifresi. Verilmezse her çalıştırmada rastgele üretilir.
- `REMOTE_FRIEND_AUTO_ACCEPT=1`: operatör onayını kapatır; yalnızca kontrollü test ortamında kullan.
- `RF_FPS`: H.264 hedef FPS, `5..30`, varsayılan `30`.
- `RF_MAX_WIDTH`: gönderilen görüntünün azami genişliği, `640..3840`, varsayılan `1920`.
- `RF_BITRATE_BPS`: H.264 hedef bit hızı, varsayılan `6000000`.
- `RF_JPEG_FPS`: JPEG fallback FPS, `2..15`, varsayılan `10`.
- `RF_JPEG_Q`: JPEG kalite, `30..95`, varsayılan `68`.
- `RF_NATIVE_BIND`: varsayılan `0.0.0.0`.
- `RF_WEB_BIND`: varsayılan `0.0.0.0`.
- `RF_HTTP_PORT`: varsayılan `33201`.
- `RF_MAX_NATIVE_SESSIONS`: varsayılan `8`.
- `RF_MAX_WEB_SESSIONS`: varsayılan `4`.
- `RF_MAX_FILE_BYTES`: varsayılan `512 MiB`.
- `REMOTE_FRIEND_DIR`: gelen dosya dizini; varsayılan kullanıcı config dizinindeki `received` klasörü.

### İnternet/VPS

- `RF_RV_SERVER`: `sunucu:33202`.
- `RF_RV_FP`: gerekmez; ilk bağlanışta parmak izi sorulup hatırlanır (TOFU).
  Yalnızca manuel sabitlemek istersen VPS sertifikasının tam SHA-256 fingerprint'i.
- `RF_PLAIN_OK=1`: yalnızca kontrollü yerel test için düz rendezvous bağlantısı; internette kullanma.
- `RF_ALLOWED_ORIGIN`: gerekirse virgülle ayrılmış ek izinli WebSocket origin'leri.
- `RF_MAX_PENDING`: relay'de onay bekleyen toplam oturum sınırı, varsayılan `256`.
- `RF_MAX_NATIVE_CONNECTIONS`: relay native bağlantı sınırı, varsayılan `128`.
- Relay `RF_MAX_WEB_SESSIONS`: varsayılan `64`.

## Uyumluluk

Protokol sürümü `2` ve transport nesli `4` oldu. Eski v0.5.5 binary'leriyle karıştırma; **host, client ve rendezvous'u birlikte değiştir**.

## Derleme

```bash
cargo build --release -p remote-friend-host -p remote-friend-client -p remote-friend-rendezvous
```

OpenH264 derlemesinde `nasm` bulunması performansa yardımcı olur. Yalnızca aynı/uyumlu CPU'larda çalıştırılacak yerel build için ayrıca `RUSTFLAGS="-C target-cpu=native"` kullanılabilir; genel dağıtım binary'sinde kullanma.
