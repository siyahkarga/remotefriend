# RemoteFriend Güvenlik Notları

## Tehdit modeli

RemoteFriend, parola bilen ve host tarafından onaylanan bir istemciye ekran, input ve dosya aktarımı yetkisi verir. Bu yetki pratikte bilgisayar başında oturmak kadar güçlüdür. Şifreyi paylaşma, host terminalini açık bırakma ve `REMOTE_FRIEND_AUTO_ACCEPT=1` ayarını günlük kullanımda açma.

## Hangi hatlar şifreli?

| Yol | Şifreleme | Kullanım |
|---|---|---|
| LAN native `33200` | Yok | Yalnızca güvenilir LAN veya VPN |
| LAN web `33201` | Varsayılan kurulumda HTTP/WS | Yalnızca güvenilir LAN; HTTPS ters proxy tercih edilir |
| Native internet `33202` | İstemci/host → VPS TLS + fingerprint pinning | Güvenilir VPS gerekir; E2E değildir |
| VPS web | Kurulum betiği Let's Encrypt HTTPS kurar; host dial-back TLS VPS'de sonlanır | Güvenilir VPS gerekir; E2E değildir |

VPS relay trafik içeriğini iletmeden önce TLS'yi sonlandırır. Bu nedenle relay sunucusu parolayı, ekranı, input'u ve dosyaları görebilir. Sunucu ele geçirilirse gizlilik kaybolur. Gerçek uçtan uca gizlilik bu sürümün kapsamı dışındadır.

## v0.5.6 ile düzeltilen yüksek riskli sorunlar

- Tarayıcı → VPS yolundaki parola artık hosta iletilip doğrulanıyor.
- Özel rustls doğrulayıcı artık TLS 1.2/1.3 handshake imzalarını gerçekten doğruluyor.
- 9 haneli ID tek başına host kaydı için yeterli değil; kalıcı 256 bit host sırrı gerekiyor.
- Varsayılan `1234` kaldırıldı; şifre verilmezse rastgele oturum şifresi üretiliyor.
- WebSocket `Origin` doğrulaması eklendi.
- Mesaj, bağlantı, dosya boyutu, chunk, eşzamanlı transfer ve oturum limitleri eklendi.
- Gelen dosyalar tahmin edilebilir `/tmp` adlarına ve rastgele offsetlere yazılmıyor; sıralı chunk, güvenli ad, `.part` ve atomik rename kullanılıyor.
- Tarayıcı parolası `localStorage` içinde tutulmuyor.
- Host kayıt sırrı ve ayarlar Unix'te `0600`, config dizini `0700` izinle yazılıyor.

## v0.6.2 ile eklenenler

- Hatalı parola kilidi (host'ta, tüm yollar için): 60 sn içinde 5 hata → 1 dk, tekrarında 2/4/8/16 dk.
  Kilit süresince parola hiç değerlendirilmez.
- Röle sunucusunda IP başına dakikada 12 bağlantı isteği sınırı (nginx arkasında `X-Real-IP` yalnızca
  yerel proxy'den kabul edilir).
- VPS kurulumunda otomatik HTTPS (Let's Encrypt; alan adı yoksa sslip.io): tarayıcı yolunda parola ve
  görüntü artık internette düz metin gitmez.
- TLS anahtarı systemd `LoadCredential=` ile verilir; anahtar dosyası servis kullanıcısına açılmak zorunda değil.
- Wayland'da girdi masaüstünün RemoteDesktop portalıyla verilir (kullanıcı onayı, iptal edilebilir).
- Oturum biterken basılı kalan tuş/fare düğmeleri host'ta otomatik bırakılır.
- Sıkı CSP (`default-src 'none'`), `X-Frame-Options: DENY`, `Permissions-Policy`.
- Onay soruları tek bir stdin okuyucusundan geçer (zaman aşımına uğrayan soru sonraki cevabı çalamaz).
- Üretilen şifreler okunaklı (`abcde-23456`, ~50 bit); kilit ile çevrimiçi tahmin pratikte imkânsız.
  Şifre `~/.config/remotefriend/password` (0600) içinde kalıcıdır; `--new-password` ile yenilenir.
- Güvenilir cihaz: operatör **K** ile onaylarsa tarayıcıya 256 bit belirteç verilir, host yalnızca SHA-256
  özetini saklar (`trusted_devices.json`, 0600). Belirteç şifrenin yerine geçmez, yalnızca onayı atlar.
  `--forget-devices` ile hepsi iptal edilir.
- Kopan oturum için tek kullanımlık, 10 dk geçerli yeniden bağlanma belirteci (yine şifre gerekir).

## Güvenli dağıtım

- `33200` ve `33201` portlarını WAN'a yönlendirme.
- VPS web arayüzünü `127.0.0.1:33203` üzerinde tutup Nginx/HTTPS arkasında yayınla.
- `33202` için TLS sertifikasının SHA-256 fingerprint'ini host ve native client üzerinde pinle.
- VPS'de ayrı `remotefriend` sistem kullanıcısı, `UMask=0077` ve systemd hardening kullan.
- Host kayıt dosyasını (`/var/lib/remotefriend/hosts.json`) yedekle ve gizli tut.
- Host cihazındaki `~/.config/remotefriend/host_secret` kaybolursa aynı ID yeniden kaydolamaz; VPS yöneticisi ilgili registry kaydını manuel kaldırmalıdır.

## Bilinen kalan riskler

- Relay uçtan uca şifreli değil.
- LAN native ve varsayılan LAN web trafiği şifreli değil.
- Kilit host genelindedir: ID'yi bilen biri bilerek hatalı deneme yaparak meşru kullanıcıyı kısa süreliğine
  kilitleyebilir (hizmet engelleme). Bu, tahmin saldırısına karşı bilinçli bir takastır.
- Ekran yakalama, input enjeksiyonu ve dosya aktarımı işletim sistemi kullanıcısının yetkileriyle çalışır.
- Windows'ta hassas dosya izinleri Unix `0600` semantiğiyle birebir uygulanmaz; kullanıcı profilinin NTFS ACL'leri korunmalıdır.

Güvenlik açığı bildirimlerinde parola, host secret, TLS private key, ekran görüntüsü veya gerçek kişisel dosya ekleme.
