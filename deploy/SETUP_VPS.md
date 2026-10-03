# VPS kurulumu (Ubuntu/Debian)

> Güvenlik sınırı: röle uçtan uca şifreli değildir. VPS'i yöneten ya da ele geçiren kişi görüntü,
> girdi, parola ve dosya içeriğini teknik olarak görebilir. Yalnızca kendi/güvendiğin sunucuyu kullan.

## Tek komut (kurulum ve güncelleme)

```bash
curl -sL https://raw.githubusercontent.com/siyahkarga/remotefriend/main/deploy/setup-vps.sh | sudo bash
```

Betik şunları yapar (tekrar çalıştırmak güvenlidir):

1. `nginx`, `certbot`, `ufw` kurar; `remotefriend` sistem kullanıcısını oluşturur.
2. Son sürüm `remote-friend-rendezvous` dosyasını indirir (önce servisi durdurur, atomik değiştirir).
3. Röle TLS sertifikasını (`/opt/remotefriend/cert.pem`, port 33202) **korur**; yoksa üretir.
4. Dosya izinlerini her seferinde onarır.
5. systemd servisini kurar. Servis sertifikayı `LoadCredential=` ile alır: systemd dosyayı root
   olarak okuyup servise verir, dosya izinleri yüzünden servis düşmez.
6. nginx'i kurar ve **Let's Encrypt HTTPS** alır. Alan adın yoksa `<ip-tireli>.sslip.io` kullanılır
   (ör. `169-58-37-61.sslip.io`, ayar gerekmez). Sertifika otomatik yenilenir.
7. Güvenlik duvarında 80, 443 ve 33202'yi açar; sonunda web adresini ve parmak izini yazar.

Seçenekler:

```bash
# kendi alan adın (A kaydı bu VPS'i göstermeli)
curl -sL .../setup-vps.sh | sudo DOMAIN=uzak.ornek.com EMAIL=sen@ornek.com bash
# IP otomatik bulunamazsa
curl -sL .../setup-vps.sh | sudo SERVER_IP=1.2.3.4 bash
# belirli sürüm
curl -sL .../setup-vps.sh | sudo VERSION=v0.6.2 bash
```

Bulut sağlayıcının güvenlik grubunda da **80, 443 ve 33202/TCP** açık olmalı (80, sertifika alımı için).

## Host ayarı

Host başlatıcısı sunucuyu bir kez sorar: `VPS_IP:33202`. İlk bağlanışta host terminali parmak
izini sorar; kurulumun sonunda yazan parmak iziyle aynıysa **E** de. Telefonda kurulumun yazdığı
`https://…` adresini aç.

## Sorun giderme

### `Permission denied (os error 13)` ile servis sürekli yeniden başlıyor

Neden: servis kullanıcısı (`remotefriend`) TLS anahtarını ya da kayıt dosyasını okuyamıyor
(ör. sertifika elle yeniden üretildi / kopyalandı ve sahibi `root`, izni `600` kaldı).

En kolayı kurulum betiğini yeniden çalıştırmak. Elle düzeltmek için **her komutu ayrı satırda** çalıştır:

```bash
sudo curl -fsSL https://raw.githubusercontent.com/siyahkarga/remotefriend/main/deploy/remotefriend.service -o /etc/systemd/system/remotefriend.service
sudo chown root:remotefriend /opt/remotefriend /opt/remotefriend/cert.pem /opt/remotefriend/key.pem
sudo chmod 750 /opt/remotefriend
sudo chmod 640 /opt/remotefriend/key.pem
sudo chown -R remotefriend:remotefriend /var/lib/remotefriend
sudo chmod 700 /var/lib/remotefriend
sudo systemctl daemon-reload
sudo systemctl restart remotefriend
sudo journalctl -u remotefriend -n 30 --no-pager
```

Hangi dosyanın okunamadığını görmek için günlüğün **tamamına** bak (`-n 30`); hata satırının
üstünde `sertifika açılamadı: …`, `anahtar açılamadı: …` ya da `host kayıt dosyası okunamadı: …` yazar.

### HTTPS alınamadı

`/tmp/rf-certbot.log` dosyasına bak. En sık neden: 80 numaralı port bulut güvenlik grubunda kapalı
ya da başka bir web sunucusu 80'i kullanıyor. Düzeltip betiği yeniden çalıştır.

### "bu ID başka bir host anahtarına kayıtlı"

Host'un `~/.config/remotefriend/host_secret` dosyası değişti/silindi. Eski kaydı kaldır:

```bash
sudo systemctl stop remotefriend
sudo nano /var/lib/remotefriend/hosts.json   # ilgili 9 haneli ID satırını sil
sudo systemctl start remotefriend
```

## Bakım

- `/var/lib/remotefriend/hosts.json` dosyasını gizli tut ve yedekle (host kimlik sırları içerir).
- Röle sertifikası değişirse host'lar parmak izini yeniden sorar; yeni parmak izini güvenli kanaldan doğrula.
- `RF_PLAIN_OK=1` değerini üretim sunucusunda kullanma.
