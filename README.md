# RemoteFriend

Telefondan veya başka bir bilgisayardan kendi bilgisayarına bağlanmak için uzak masaüstü.
Görüntü H.264 ile akar; fare, klavye, dokunmatik ve dosya gönderme desteklenir.

| Paylaşılan bilgisayar (host) | Durum |
|---|---|
| Linux **Wayland** (GNOME, KDE) | Ekran + fare/klavye: masaüstü portalı (bir kez izin sorar) |
| Linux **X11** | Ekran + fare/klavye |
| **Windows** 10/11 | Ekran + fare/klavye (yüksek DPI ölçekli ekranlar dahil) |
| **macOS** 12+ (Apple Silicon/Intel) | Ekran Kaydı + Erişilebilirlik izni gerekir |

Bağlanan taraf: herhangi bir güncel tarayıcı (telefon dahil, kurulum yok) ya da `remote-friend-client`.

## Hızlı başlangıç

### 1. Sunucu (VPS) — bir kez

Ubuntu/Debian VPS'te:

```bash
curl -sL https://raw.githubusercontent.com/siyahkarga/remotefriend/main/deploy/setup-vps.sh | sudo bash
```

Betik her şeyi kurar ve sonunda **web adresini** (`https://1-2-3-4.sslip.io` gibi, alan adı
gerekmez, otomatik HTTPS) ve **parmak izini** yazar. Güncellemek için aynı komutu tekrar çalıştır.
Ayrıntı: [deploy/SETUP_VPS.md](deploy/SETUP_VPS.md).

### 2. Paylaşılacak bilgisayar (host)

| Sistem | Çalıştır |
|---|---|
| Linux | `./start_remotefriend_linux.sh` |
| Windows | `start_remotefriend_win64.bat` (çift tık) |
| macOS | `start_remotefriend_macos.command` (çift tık) |

İlk çalıştırmada sunucu adresi sorulur (`1.2.3.4:33202`; sadece yerel ağ için boş bırak).
Başlatıcılar yeni sürüm çıkınca programı kendisi günceller. Ekranda şuna benzer bir kutu çıkar:

```
  RemoteFriend Host  ·  ev-bilgisayari
  Bilgisayar kodu : 123 456 789
  Şifre           : k7mpx-29qra
  İnternetten     : https://1-2-3-4.sslip.io
  Yerel ağdan     : http://192.168.1.20:33201
```

- **Wayland:** ilk açılışta masaüstü "uzak masaüstü / ekran paylaşımı" izni ister → *Paylaş* de.
  İzin hatırlanır; bir daha sorulmaz.
- **macOS:** Sistem Ayarları → Gizlilik ve Güvenlik → **Ekran Kaydı** ve **Erişilebilirlik**
  listelerinde Terminal'i aç, sonra başlatıcıyı tekrar çalıştır.

### 3. Bağlan

Telefonda/tarayıcıda web adresini aç → bilgisayar kodu + şifre → **Bağlan**.
Host'ta terminalde **E** ile onayla (masaüstü bildirimi de gelir).

## Telefonda kullanım

| Mod | Hareket |
|---|---|
| **Touchpad** (varsayılan) | Kaydır: imleç · Dokun: tıkla · İki parmakla dokun: sağ tık · İki parmakla kaydır: kaydırma · Basılı tut + sürükle: sürükle-bırak |
| **Dokunmatik ekran** | Dokunduğun yer tıklanır · Basılı tut: sağ tık · Sürükle: sürükle-bırak · İki parmak: kaydır / yakınken gezin |

Her iki modda iki parmakla **aç/kapa = yakınlaştır**. Üst çubuk:
⌨ telefon klavyesi · ⌘ Ctrl/Alt/Win, Esc, oklar, F tuşları, Kopyala/Yapıştır, panodaki metni yaz ·
⚙ dokunmatik modu ve **kalite** (Hızlı / Dengeli / Net) · ⤒ dosya gönder (host'ta *İndirilenler/RemoteFriend*) · ⛶ tam ekran.

## Görüntü kalitesi ve hız

- Tarayıcıda H.264 (WebCodecs) kullanılır; bunun için sayfa **HTTPS** olmalı (VPS betiği kurar).
  HTTPS yoksa sayfa yavaş JPEG moduna düşer ve bunu girişte söyler.
- Kalite canlı değiştirilebilir: **Hızlı** (1280 px, düşük bit hızı, mobil veri),
  **Dengeli** (1920 px, 8 Mbit/s), **Net** (tam çözünürlük, 16 Mbit/s).
- Ağ yavaşlarsa host bit hızını otomatik düşürür; gecikme birikmez (istemci her kareyi onaylar,
  fazlası kaynakta atlanır). Ekran değişmiyorsa veri gönderilmez.

Ortam değişkenleri ("Dengeli" profili ayarlar): `RF_QUALITY=fast|balanced|sharp`, `RF_FPS` (5–60, 30),
`RF_MAX_WIDTH` (1920), `RF_BITRATE_BPS` (8000000), `RF_JPEG_Q` (70), `RF_JPEG_FPS` (10).

## Güvenlik

- Her bağlantı **şifre + host'ta onay (E)** ister. Gözetimsiz kullanım için `REMOTE_FRIEND_AUTO_ACCEPT=1`
  (yalnızca güçlü, kalıcı `REMOTE_FRIEND_PASS` ile).
- Şifre her açılışta yeniden üretilir (`abcde-23456` biçiminde, ~50 bit; büyük/küçük harf ve tire önemsiz).
  Kalıcı şifre için `REMOTE_FRIEND_PASS` ayarla (en az 10 karakter).
- Hatalı şifre kilidi: 60 sn'de 5 hata → 1 dk kilit, tekrarında 2/4/8/16 dk. Sunucu ayrıca IP başına
  dakikada 12 bağlantı isteği sınırı uygular.
- İnternet yolu: tarayıcı ↔ VPS HTTPS, host ↔ VPS TLS + parmak izi sabitleme. **Röle uçtan uca şifreli
  değildir**: VPS'i yöneten kişi trafiği görebilir — sadece kendi/güvendiğin VPS'i kullan.
- Yerel ağ yolu (`:33200`, `:33201`) şifresizdir; bu portları internete açma.

Ayrıntı: [SECURITY.md](SECURITY.md).

## Sorun giderme

| Belirti | Çözüm |
|---|---|
| VPS'te `Permission denied (os error 13)` | Kurulum betiğini yeniden çalıştır (servis artık sertifikayı systemd üzerinden alır). Bkz. [SETUP_VPS.md](deploy/SETUP_VPS.md#sorun-giderme) |
| "ID bulunamadı (host çevrimiçi değil)" | Host çalışıyor mu, `RF_RV_SERVER` doğru mu? Host terminalinde ">>> İnternet sunucusuna bağlandı" yazmalı |
| Wayland'da görüntü yok | Host terminalinde izin uyarısı var mı? Masaüstündeki izin penceresini onayla; reddettiysen host'u yeniden başlat |
| Wayland'da fare/klavye çalışmıyor | Masaüstü RemoteDesktop portalını desteklemeli (GNOME 41+, KDE 5.27+). wlroots (Sway/Hyprland) yalnız XWayland pencerelerini kontrol eder |
| macOS'ta siyah ekran / kontrol yok | Ekran Kaydı / Erişilebilirlik izinleri (yukarıda) |
| Görüntü yavaş, "JPEG modu" | Sayfayı HTTPS adresinden aç; güncel Chrome/Safari/Edge kullan |
| "sürüm uyumsuz" | Host, istemci ve VPS'i aynı sürüme güncelle (başlatıcılar ve VPS betiği bunu yapar) |

## Derleme

```bash
# Linux: libpipewire-0.3-dev libspa-0.2-dev clang libclang-dev libxkbcommon-dev libgtk-3-dev gerekir
cargo build --release -p remote-friend-host -p remote-friend-client -p remote-friend-rendezvous
cargo test -p remote-friend-common -p remote-friend-host
```

Geliştirmede: `RF_TEST_PATTERN=1` (sentetik görüntü, ekran izni gerekmez), `RF_INPUT_DRY=1`
(girdileri uygulamak yerine günlüğe yazar), `RF_NATIVE_PORT` / `RF_HTTP_PORT` (ikinci bir host için).

Protokol sürümü **3**: host, istemci ve VPS birlikte güncellenmelidir.
