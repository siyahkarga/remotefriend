# RemoteFriend Performans Rehberi

## v0.5.5 neden 3–5 FPS'e düşebiliyordu?

1. Hedef 15 FPS'ti ve capture/encode/send tamamlandıktan sonra sabit 66 ms daha uyunuyordu. Gerçek FPS yaklaşık `1 / (işlem süresi + 66 ms)` oluyordu.
2. LAN tarayıcı sayfası düz HTTP'de açıldığında WebCodecs çoğu zaman kullanılamıyor, CPU ağırlıklı tam-kare JPEG fallback devreye giriyordu.
3. Her izleyici ekranı yeniden yakalayıp yeniden kodluyordu.
4. İstemci her UI turunda aynı RGBA kareyi clone edip GPU texture'ına yeniden yüklüyordu.
5. Video ve input kuyrukları sınırsız veya gecikmeyi büyütecek biçimdeydi.
6. RGBA → RGB → YUV420 ve yazılımsal OpenH264 hâlâ CPU maliyetlidir.

## v0.5.6 değişiklikleri

- Sabit zamanlı 30 FPS pacing; işlem gecikince ek uyku ve gecikme birikimi yok.
- Tek global H.264 capture/encode hattı; tüm izleyiciler aynı kodlanmış kareyi alır.
- Tek global JPEG fallback hattı.
- Broadcast kuyrukları küçük; yavaş istemci eski kareleri atlayıp güncel kareye gelir.
- H.264 keyframe aralığı yaklaşık 1 saniye; yeni izleyici hızlı toparlanır.
- OpenH264 auto-thread, screen-content realtime, frame-skip ve 6 Mbit/s varsayılan profil.
- xcap monitör seçimi her karede yeniden yapılmıyor.
- İstemci RGBA kareyi clone etmeden tüketiyor ve GPU texture'ını yalnızca yeni kare geldiğinde güncelliyor.
- Tarayıcı H.264 decode kuyruğu büyürse eski zinciri bırakıp sonraki keyframe'i bekliyor.
- JPEG decode yalnızca en yeni bekleyen kareyi tutuyor.
- Input ve ağ kanalları bounded; eski mouse hareketleri gerektiğinde düşüyor.


## Darboğazı terminalden ölçme

Aktif bir izleyici varken host her yaklaşık 5 saniyede şu satırı yazar:

```text
video ölçüm: 27.8 fps, capture 4.2 ms, encode 27.1 ms, 1600x900, izleyici 1
```

- `encode` 33 ms'nin belirgin biçimde üstündeyse 30 FPS'i yazılımsal OpenH264 sınırlıyor; önce `RF_MAX_WIDTH=1280` dene.
- `capture` yüksekse ekran yakalama yolu sınırlıyor; Wayland'da PipeWire izninin verildiğini ve `pipewire format` logunun geldiğini kontrol et.
- Host ölçümü yüksek, istemci FPS'i düşükse istemci decode/GPU texture yüklemesi veya ağ gecikmesi sınırlıyor.
- Tarayıcı meta satırında `JPEG` yazıyorsa HTTPS/WebCodecs yerine fallback kullanılıyordur.

## Profil seçimi

### Dengeli

```text
RF_FPS=30
RF_MAX_WIDTH=1600
RF_BITRATE_BPS=6000000
```

### Daha düşük CPU ve internet kullanımı

```text
RF_FPS=20
RF_MAX_WIDTH=1280
RF_BITRATE_BPS=4000000
```

### 1080p netlik

```text
RF_FPS=30
RF_MAX_WIDTH=1920
RF_BITRATE_BPS=8000000
```

Bu profil güçlü CPU ister. Video oynatımı gibi tüm ekranın sürekli değiştiği sahneler, statik masaüstünden çok daha ağırdır.

### Düz HTTP tarayıcı/JPEG

```text
RF_MAX_WIDTH=1280
RF_JPEG_FPS=8
RF_JPEG_Q=62
```

JPEG modunda 30 FPS bekleme. Akıcılık için native client veya geçerli HTTPS/WSS altında WebCodecs H.264 kullan.

## Derleme ayarları

- Release build kullan: `cargo build --release`.
- `nasm` kuruluysa OpenH264 SIMD/assembly yolu kullanılabilir.
- Aynı bilgisayar/CPU ailesi için yerel derlemede `RUSTFLAGS="-C target-cpu=native"` denenebilir.
- Genel dağıtım binary'sinde `target-cpu=native` kullanma; başka CPU'larda açılmayabilir.

## Bir sonraki büyük performans adımı

Bu hotfix yazılımsal H.264 mimarisini iyileştirir; fakat gerçek 1080p60/1440p60 hedefi için sıfır-kopyaya yakın bir yol gerekir:

1. Windows Graphics Capture veya DXGI Desktop Duplication.
2. GPU yüzeyini doğrudan NVENC/QSV/AMF'ye verme.
3. Linux'ta PipeWire DMA-BUF + VAAPI/NVENC.
4. WebRTC ile congestion control, jitter buffer, DTLS-SRTP ve donanımsal tarayıcı decode.
5. Input için ayrı düşük gecikmeli data channel; dosya için kontrollü backpressure.

NVIDIA GPU bulunan bir hostta NVENC, CPU OpenH264'a göre genellikle en büyük performans kazancını sağlayan adımdır. Bu, küçük bir ayar değil; capture yüzeyi, renk formatı, encoder ve transport katmanının birlikte değiştirilmesini gerektirir.
