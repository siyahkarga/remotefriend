# RemoteFriend v0.5.5 — Performans ve Güvenlik İncelemesi

**İnceleme tarihi:** 2 Ekim 2026
**İncelenen paket:** `remotefriend-v0.5.5-kaynak.zip`
**Hazırlanan düzeltme:** `remotefriend-v0.5.6-hotfix-kaynak`
**Orijinal ZIP SHA-256:** `5e45870fbc92bf523f1e02f2116cf76e7b9541d6014fc7174c916365587fd4bd`

## 1. Sonuç

3–5 FPS sorunu tek bir nedenden değil, birbirini büyüten birkaç tasarım hatasından geliyor:

1. Orijinal host en fazla yaklaşık **15 FPS** için ayarlanmış (`FPS_MS = 66`). Üstelik 66 ms bekleme, capture ve encode işlemlerinden **sonra** yapılıyor. Dolayısıyla gerçek kare süresi `capture + encode + ağ yazımı + 66 ms` oluyor. İşlem kısmı 130–250 ms sürerse sonuç doğal olarak yaklaşık 3–5 FPS oluyor.
2. LAN tarayıcı sayfası çoğunlukla `http://IP:33201` üzerinden açılıyor. WebCodecs `VideoDecoder` güvenli bağlam gerektirdiği için LAN IP’sindeki düz HTTP sayfasında çoğu tarayıcı H.264 yolunu kullanamıyor; uygulama ağır JPEG moduna düşüyor.
3. Her izleyici kendi ekran yakalama ve kendi H.264/JPEG encode döngüsünü başlatıyor. İkinci istemci CPU yükünü yaklaşık ikiye katlayabiliyor.
4. Native istemci, yeni kare gelmese bile aynı tam görüntüyü tekrar klonlayıp GPU texture’ına yüklüyor.
5. İlk kare geldiğinde aynı `Mutex` kilidi bırakılmadan ikinci kez alınmaya çalışılıyor. Bu, ilk görüntüde kilitlenmeye yol açabilecek gerçek bir deadlock hatası.
6. Yazılımsal OpenH264, RGBA → RGB → YUV dönüşümleri ve tam kare kopyaları CPU’da çalışıyor. Yüksek çözünürlükte bu mimari zaten sınıra çabuk geliyor.

Hazırlanan v0.5.6 hotfix bu sorunların düşük riskle düzeltilebilen kısmını ele alıyor: hedef 30 FPS, sabit zamanlı pacing, tek ortak capture/encode hattı, gecikme biriktirmeyen küçük broadcast kuyruğu, yalnızca yeni karede texture güncelleme, deadlock düzeltmesi, JPEG için ortak yayın hattı ve tarayıcı decode backpressure.

**Beklenti:** Bu değişiklikler 3–5 FPS durumunu belirgin biçimde iyileştirmelidir. Bununla birlikte 1080p/30’da nihai sonuç host CPU’suna, ekran yakalama backend’ine, ağ bant genişliğine ve yazılımsal OpenH264 hızına bağlıdır. Paket bu inceleme ortamında Rust derleyicisi bulunmadığı için derlenemedi; bu nedenle “kesin 30 FPS” garantisi verilmemektedir.

---

## 2. İnceleme yöntemi ve sınırlar

Yapılanlar:

- Tüm Rust, HTML/JavaScript, TOML, YAML, batch ve systemd dosyaları statik olarak incelendi.
- Video capture → renk dönüşümü → encode → kuyruk → ağ → decode → texture yükleme zinciri takip edildi.
- LAN, tarayıcı, native istemci ve VPS rendezvous/relay yolları ayrı ayrı incelendi.
- Kimlik doğrulama, TLS verifier, WebSocket Origin, dosya aktarımı, mesaj sınırları ve oturum yönetimi kontrol edildi.
- Kod üzerinde performans ve güvenlik yamaları uygulandı.
- 25 maddelik otomatik statik doğrulama çalıştırıldı; 25/25 geçti.

Sınırlar:

- Ortamda `cargo`, `rustc` ve Rust bağımlılık önbelleği yoktu.
- İnternet erişimi kapalı olduğu için geçici Rust araç zinciri indirilemedi.
- Bu nedenle `cargo check`, `cargo test`, gerçek iki bilgisayarlı görüntü testi ve gecikme/FPS benchmark’ı çalıştırılamadı.
- Bağımlılık zafiyet taraması (`cargo audit`) çalıştırılamadı.

---

## 3. Performans bulguları

### P1 — Hatalı kare zamanlaması

**Orijinal kod:** `crates/host/src/main.rs:17`, `:220–269`

```rust
pub(crate) const FPS_MS: u64 = 66; // ~15fps
...
capture + encode + send
...
sleep(66ms)
```

Bu yapı 15 FPS sınırı koymakla kalmıyor; capture/encode süresini kare bütçesine ekliyor. Örneğin:

- Capture + encode + gönderim 140 ms
- Ek uyku 66 ms
- Toplam 206 ms/kare
- Sonuç yaklaşık 4.85 FPS

**Hotfix:** `RF_FPS=30` varsayılanı ve deadline tabanlı sabit pacing. İşlem süresi hedef kare aralığından kısa ise sadece kalan süre bekleniyor; uzun ise fazladan 33/66 ms eklenmiyor.

### P2 — LAN tarayıcıda WebCodecs yerine JPEG’e düşme

**Orijinal kod:** `crates/common/src/webapp.html:201–219`

Orijinal kontrol yalnızca `typeof VideoDecoder === 'undefined'` idi. Ancak LAN IP’si üzerinden düz HTTP sayfası güvenli bağlam değildir. Modern tarayıcılar `VideoDecoder` erişimini güvenli bağlamla sınırlar. Sonuçta uygulama JPEG yolunu seçer veya H.264 decoder açamaz.

JPEG yolu her kareyi CPU’da tek resim olarak sıkıştırıp tarayıcıda tekrar resim olarak açar. Masaüstü görüntüsü gibi sürekli değişen yüksek çözünürlüklü içerikte 3–5 FPS görülmesi mümkündür.

**Hotfix:** `window.isSecureContext` açıkça kontrol ediliyor. Güvensiz bağlamda davranış anlaşılır biçimde JPEG olarak işaretleniyor. JPEG yolu 10 FPS/kalite 68 varsayılanına çekildi ve yalnızca en yeni kare tutuluyor.

**Doğru kullanım:** En yüksek performans için native istemci; tarayıcı gerekiyorsa HTTPS/WSS reverse proxy.

### P3 — Her istemci için tekrar capture ve encode

**Orijinal kod:**

- Native: `crates/host/src/main.rs:220–269`
- Yerel web H.264: `crates/host/src/web.rs:92–130`
- Relay H.264: `crates/host/src/web.rs:215–249`
- Relay JPEG: `crates/host/src/web.rs:270–298`

Her bağlantı yeni bir capture ve encoder döngüsü açıyordu. Bu, CPU yükünü istemci sayısıyla çarpıyordu.

**Hotfix:** Bir ortak H.264 üreticisi ve bir ortak JPEG üreticisi var. İstemciler aynı encoded kareyi paylaşır. Broadcast kuyruğu H.264 için 3, JPEG için 2 kare ile sınırlıdır; yavaş istemci sistemi geriye çekmez.

### P4 — Eski karelerin birikmesi ve gecikmenin büyümesi

Orijinal native çıkış kanalları sınırsızdı; tarayıcı decoder kuyruğu için de backpressure yoktu. Ağ veya decode kısa süre yavaşladığında kullanıcı “düşük FPS” görmenin yanında saniyeler geriden gelen görüntü de görebilirdi.

**Hotfix:**

- Native giriş kuyruğu bounded.
- Video broadcast kuyruğu küçük ve “latest frame” mantıklı.
- Lag alan istemci eski kareleri atlar.
- WebCodecs `decodeQueueSize > 2` olduğunda decoder sıfırlanır ve sonraki IDR beklenir.
- JPEG decode meşgulse yalnızca en son bekleyen JPEG saklanır.

### P5 — Native istemcide gereksiz tam kare klonu/GPU upload

**Orijinal kod:** `crates/client/src/main.rs:587–639`

Her UI yenilemesinde `ColorImage` klonlanıyor ve texture tekrar `set()` ediliyordu. UI de 66 ms’de bir yenileniyordu. Bu, özellikle 1080p RGBA karelerde büyük bellek bant genişliği tüketir.

**Hotfix:** UI gelen kareyi `take()` ile yalnızca bir kez tüketir; texture yalnızca yeni kare olduğunda güncellenir. `TextureHandle` klonu yalnızca hafif bir referans klonudur.

### P6 — İlk karede olası deadlock

**Orijinal kod:** `crates/client/src/main.rs:248–265`

```rust
let mut s = shared.lock().unwrap();
...
shared.lock().unwrap().status = "bağlı".into();
```

Aynı thread, ilk kilidi bırakmadan aynı `std::sync::Mutex`’i tekrar kilitlemeye çalışıyor. Bu reentrant olmayan mutex ile deadlock oluşturur.

**Hotfix:** Durum mevcut kilit üzerinden güncelleniyor ve ardından kilit açıkça bırakılıyor.

### P7 — Yazılımsal encode ve çoklu renk dönüşümü

**Orijinal kod:** `crates/host/src/main.rs:534–565`

Her kare:

1. ekran yakalama ile RGBA,
2. yeni bir RGB buffer’a kopya,
3. RGB → YUV420 dönüşümü,
4. yazılımsal H.264 encode

yapıyor.

**Hotfix:** OpenH264 auto-thread, 30 FPS, 6 Mbit/s, screen-content realtime ve frame-skip ayarları eklendi. Fakat gerçek sıçrama için sonraki sürümde platform donanım encoder’ı gerekir.

---

## 4. Güvenlik bulguları

| Seviye | Bulgu | Etki | Hotfix durumu |
|---|---|---|---|
| **Kritik** | İnternet tarayıcı parolası relay tarafından okunup atılıyordu | Host onayından sonra tarayıcı oturumu parola doğrulaması olmadan açılıyordu; auto-accept ile çok tehlikeli | Parola `ApprovalRequest.auth` ile hosta taşınıyor ve sabit-süreye yakın karşılaştırma ile doğrulanıyor |
| **Kritik** | Özel rustls verifier, TLS 1.2/1.3 handshake imzasını koşulsuz “geçerli” sayıyordu | Pinned sertifikayı kopyalayan fakat özel anahtara sahip olmayan saldırganın sunucuyu taklit etmesine kapı açabilirdi | Gerçek `verify_tls12_signature` / `verify_tls13_signature` çağrıları eklendi |
| **Kritik** | Rendezvous host kaydı yalnızca tahmin edilebilir 9 haneli ID’ye bağlıydı | Aynı ID ile kayıt olup yönlendirmeyi ele geçirme riski | Cihazda saklanan rastgele 256-bit host secret eklendi; register/connect-back secret ile bağlandı |
| **Yüksek** | Relay gerçek uçtan uca şifreli değil | VPS operatörü/ele geçirilmiş VPS ekranı, klavye/fare girdisini, parolayı ve dosyaları görebilir | **Mimari kalan risk**; yalnızca güvenilen VPS kullanılmalı |
| **Yüksek** | LAN native TCP ve LAN HTTP/WebSocket düz metin | Aynı ağdaki saldırgan trafiği okuyabilir/değiştirebilir | Dokümante edildi; güvenilir LAN/VPN dışında port açılmamalı |
| **Yüksek** | WebSocket `Origin` doğrulaması yoktu | Kötü niyetli web sayfası kullanıcının tarayıcısından yerel/relay WebSocket’e bağlanmayı deneyebilirdi | Same-origin allowlist kontrolü eklendi; isteğe bağlı `RF_ALLOWED_ORIGIN` |
| **Yüksek** | Varsayılan parola `1234` ve açıkça ekrana/dokümana yazılıydı | Tahmin edilmesi çok kolay | Varsayılan rastgele parola; yapılandırılmış parola log’a yazılmıyor; zayıf parola uyarısı |
| **Yüksek** | Mesajlar, kuyruklar ve oturum sayıları geniş ölçüde sınırsızdı | Bellek/CPU DoS, uzun gecikme kuyrukları | Boyut limitleri, bounded channel, semaphore ve timeout eklendi |
| **Yüksek** | Dosya alımı `/tmp/rf_<id>_<ad>` gibi tahmin edilebilir isimle, boyut/offset sınırı olmadan yazıyordu | Symlink/yerel çakışma, disk doldurma, sparse file, yarım dosyanın tamamlanmış görünmesi | Özel dizin, rastgele isim, `.part`, 512 MiB sınırı, 256 KiB chunk, sıralı offset, atomik rename |
| **Orta** | Tarayıcı parolası `localStorage` içinde kalıcı tutuluyordu | Aynı origin’de çalışan script/XSS parolayı okuyabilir | Parola artık kalıcı depoya yazılmıyor |
| **Orta** | Relay tokenları monoton sayaçtı | Tahmin edilebilir oturum tokenları ve bağlantı karıştırma riski | Rastgele `u128` token ve session kimliği |
| **Orta** | Bir relay yönü kapanınca diğer yön asılı kalabiliyordu | Kaynak tüketimi ve hayalet oturumlar | İki yön `select!` ile birlikte iptal ediliyor |
| **Orta** | Güvenlik başlıkları yoktu | Clickjacking/XSS etkisini azaltan savunma katmanları eksikti | CSP, no-store, nosniff, no-referrer ve frame-ancestors eklendi |

### Kritik ayrıntı: tarayıcı parolası gerçekten kullanılmıyordu

**Orijinal rendezvous:** `crates/rendezvous/src/main.rs:365–395`

Hello JSON içinde `password` gönderilmesine rağmen parser yalnızca `id` ve `jpeg` alanlarını alıyordu. Ardından hosta gönderilen `ApprovalRequest` içinde parola yoktu.

**Orijinal host relay web session:** `crates/host/src/main.rs:393–430`, `crates/host/src/web.rs:215–317`

Host operatörü “E” dediğinde doğrudan H.264/JPEG oturumu başlıyor; native `Handshake.password` kontrolü web yolunda hiç çalışmıyordu.

### Kritik ayrıntı: TLS imza doğrulaması bypass

**Orijinal:** `crates/common/src/tls.rs:49–64`

Hem TLS 1.2 hem TLS 1.3 callback’i doğrudan `HandshakeSignatureValid::assertion()` döndürüyordu. rustls sözleşmesine göre bu değer yalnızca imza gerçekten doğrulandıysa dönmelidir.

**Hotfix:** Sertifika fingerprint pinning korunurken sertifikanın public key’iyle gerçek handshake imzası da doğrulanıyor.

---

## 5. Hotfix’te yapılan başlıca değişiklikler

### Performans

- `RF_FPS`: 5–30, varsayılan 30.
- `RF_MAX_WIDTH`: varsayılan 1920.
- `RF_BITRATE_BPS`: varsayılan 6.000.000.
- İşlem süresini hesaba katan deadline tabanlı frame pacing.
- Tüm native/web izleyiciler için ortak H.264 capture/encode hattı.
- Tüm JPEG izleyiciler için ortak JPEG capture/encode hattı.
- Küçük broadcast kuyrukları; yavaş istemci eski kareleri atlar.
- Yeni izleyici geldiğinde IDR/SPS/PPS alabilmesi için encoder reset mekanizması.
- 5 saniyelik capture/encode/FPS ölçüm log’u.
- PipeWire tarafında en yeni buffer’ı alma; gereksiz clone azaltma.
- Native UI’de yalnızca yeni karede texture upload.
- WebCodecs/JPEG decode backpressure.
- Mouse hareketinde bounded `try_send`; input kuyruğunun video/uygulamayı boğması engellendi.

### Güvenlik

- Protokol `v2`, transport generation `4`; eski-yeni sessiz karışım engellendi.
- Rastgele 256-bit host secret ve rastgele u128 relay tokenları.
- Tarayıcı parolasının host tarafından gerçek doğrulanması.
- Gerçek TLS 1.2/1.3 handshake signature doğrulaması.
- WebSocket Origin kontrolü.
- Rastgele varsayılan oturum parolası.
- Mesaj, blob, kmsg, dosya ve oturum limitleri.
- Dosyada `.part` + `sync_all` + atomik rename.
- Config/secret dosyalarında Unix’te 0600, dizinde 0700.
- Connection/handshake timeout ve oturum semaphore’ları.
- Güvenlik başlıkları ve password’ün `localStorage`’dan kaldırılması.

---

## 6. Kullanım ayarları

### En iyi başlangıç ayarı — native istemci

Windows host terminalinde:

```bat
set RF_FPS=30
set RF_MAX_WIDTH=1920
set RF_BITRATE_BPS=6000000
HOST_BASLAT.bat
```

CPU kullanımı yüksek veya kareler yetişmiyorsa:

```bat
set RF_FPS=20
set RF_MAX_WIDTH=1600
set RF_BITRATE_BPS=4500000
HOST_BASLAT.bat
```

Daha zayıf host / düşük upload:

```bat
set RF_FPS=20
set RF_MAX_WIDTH=1280
set RF_BITRATE_BPS=3000000
HOST_BASLAT.bat
```

### Tarayıcı istemci

- `http://LAN-IP:33201` kullanımında büyük olasılıkla JPEG görülür.
- Arayüzde codec göstergesi **JPEG** ise 30 FPS beklenmemeli.
- H.264/WebCodecs için HTTPS/WSS kullanın.
- Tarayıcı yerine native istemci kalite ve gecikme açısından daha doğru tercih.

### Log’u yorumlama

Host yaklaşık her 5 saniyede şuna benzer ölçüm basar:

```text
video ölçüm: 27.8 fps, capture 8.2 ms, encode 18.5 ms, ...
```

- `capture + encode < 33 ms`: 30 FPS teorik olarak mümkün.
- `encode > 33 ms`: çözünürlüğü düşürün veya donanım encoder’a geçin.
- Host 30 FPS üretip istemci düşük gösteriyorsa sorun ağ/decode/UI tarafındadır.
- Codec JPEG ise önce HTTPS/native istemci sorununu çözün.

---

## 7. Kalan riskler ve henüz yapılmayanlar

1. **Relay uçtan uca şifreli değil.** Hotfix TLS verifier’ı düzeltir ama VPS’nin plaintext’i görmesini engellemez.
2. **LAN trafiği şifreli değil.** 33200/33201 portlarını internete yönlendirmeyin.
3. **Donanım encoder yok.** RTX 4090’daki NVENC kullanılmıyor; OpenH264 CPU’da çalışıyor.
4. H.264 ve JPEG izleyici aynı anda bağlıysa iki ayrı capture üreticisi rekabet edebilir.
5. Native decode ve YUV → RGBA dönüşümü network task’ında yapılıyor; ayrı decode worker daha iyi olur.
6. Her native izleyiciye encoded `Vec` için en az bir kopya hâlâ yapılabilir.
7. Kalıcı IP-temelli rate limiter/account lockout yok; yalnızca gecikme, timeout ve eşzamanlılık sınırları var.
8. `REMOTE_FRIEND_AUTO_ACCEPT=1` saldırı yüzeyini ciddi büyütür; kullanılması önerilmez.
9. Relay registry host secret’ı sunucu diskinde tutar. Dosya izinleri sıkıdır fakat VPS yine güvenilir olmalıdır.
10. Inline HTML nedeniyle CSP’de `unsafe-inline` bulunuyor. Script/style ayrı dosyaya taşınırsa CSP sıkılaştırılabilir.
11. GitHub Actions sürümleri commit SHA yerine değişebilir etiketlere bağlı; supply-chain sertleştirmesi tamamlanmadı.
12. Derleyici ve gerçek uçtan uca test yapılmadı.

---

## 8. Daha iyi mimari için sonraki sürüm

### Aşama 1 — Donanım encode

RTX 4090 bulunan hostta NVENC H.264/HEVC/AV1 ile encode CPU’dan ayrılmalı. NVIDIA Video Codec SDK Windows ve Linux’ta donanım hızlandırmalı encode/decode sağlar. Düşük gecikmeli masaüstü aktarımı için H.264 low-latency preset, B-frame kapalı, kısa GOP/periodic IDR ve mümkünse capture yüzeyinden encoder’a zero-copy yol kullanılmalı.

Beklenen kazanç:

- CPU yükünde büyük düşüş,
- 1080p60/1440p60 için daha yüksek ihtimal,
- aynı bit hızında daha tutarlı kalite,
- encode süresinde daha düşük jitter.

### Aşama 2 — WebRTC taşıma

Tarayıcı hedefleniyorsa özel WebSocket video protokolü yerine WebRTC daha doğru uzun vadeli çözüm:

- DTLS-SRTP ile medya şifreleme,
- congestion control,
- packet loss/jitter yönetimi,
- NAT traversal için ICE/STUN/TURN,
- tarayıcıların donanım decode yollarıyla daha doğal entegrasyon.

Bu yine otomatik olarak “VPS asla göremez” anlamına gelmez; TURN ve signaling tasarımı ile kimlik doğrulama doğru kurulmalıdır. Fakat mevcut TLS-terminate-and-forward relay’den daha sağlam bir temel sağlar.

### Aşama 3 — Gerçek uçtan uca oturum anahtarı

- Cihaz kimlik anahtarı,
- ephemeral X25519 anahtar değişimi,
- host ekran/input/file payload’ları için AEAD,
- relay’in yalnızca şifreli paket yönlendirmesi,
- kullanıcıya iki uçta doğrulanabilir kısa güvenlik kodu.

### Aşama 4 — Capture/encode zero-copy

Windows’ta Desktop Duplication / Windows Graphics Capture → D3D11 texture → NVENC; Linux’ta PipeWire DMA-BUF → VAAPI/NVENC gibi bir yol. En büyük kalan performans kazanımı burada.

---

## 9. Doğrulama sonucu

Otomatik statik kontrol: **25 geçti, 0 uyarı, 0 hata**.

Kontrol edilenler arasında:

- Tüm Cargo TOML dosyalarının parse edilmesi.
- `Cargo.lock` yerel paket/rand/rustls tutarlılığı.
- GitHub Actions YAML parse.
- HTML parse ve yinelenen `id` kontrolü.
- Browser JavaScript için `node --check`.
- JavaScript DOM ID referanslarının HTML ile eşleşmesi.
- Tüm Rust dosyalarında yorum/dize duyarlı delimiter dengesi.
- Eski güvensiz kalıpların bulunmaması:
  - koşulsuz TLS signature assertion,
  - `unbounded_channel`,
  - sabit `1234`,
  - password `localStorage`,
  - eski 66 ms frame sleep,
  - tahmin edilebilir `/tmp` alıcı yolu.
- Yeni korumaların bulunması:
  - protocol v2,
  - host secret,
  - gerçek TLS signature verify,
  - Origin check,
  - bounded channel,
  - H.264/JPEG broadcast,
  - atomik file rename.

**Derleyici doğrulaması:** Yapılamadı. Paket, gerçek makinede önce `cargo check --workspace --locked`, ardından `cargo test --workspace --locked` ile doğrulanmalıdır.

---

## 10. Referans alınan teknik belgeler

- MDN Web Docs — `VideoDecoder`: yalnızca secure context’te kullanılabilir.
- rustls `ServerCertVerifier` dokümantasyonu: `HandshakeSignatureValid` yalnızca imza gerçekten geçerliyse döndürülmelidir.
- OWASP WebSocket Security Cheat Sheet: her WebSocket handshake’inde `Origin` allowlist doğrulaması önerilir.
- NVIDIA Video Codec SDK: NVENC/NVDEC donanım hızlandırmalı video encode/decode API’leri.
- W3C WebRTC ve IETF RFC 8827: DTLS tabanlı WebRTC güvenlik ve medya taşıma mimarisi.
