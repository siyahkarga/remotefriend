# VPS kurulumu (Ubuntu/Debian)

> Güvenlik sınırı: RemoteFriend relay bu sürümde uçtan uca şifreli değildir. VPS operatörü ve VPS'yi ele geçiren kişi görüntü, input, parola ve dosya içeriğini teknik olarak görebilir. Yalnızca güvendiğin sunucuyu kullan.

## 1. Ayrı kullanıcı ve dizinler

```bash
sudo useradd --system --home /var/lib/remotefriend --shell /usr/sbin/nologin remotefriend 2>/dev/null || true
sudo install -d -o root -g remotefriend -m 0750 /opt/remotefriend
sudo install -d -o remotefriend -g remotefriend -m 0700 /var/lib/remotefriend
```

`remote-friend-rendezvous` binary'sini `/opt/remotefriend/` içine koy:

```bash
sudo install -o root -g remotefriend -m 0750 remote-friend-rendezvous /opt/remotefriend/remote-friend-rendezvous
```

## 2. Native relay TLS sertifikası

Örnek self-signed sertifika:

```bash
sudo openssl req -x509 -newkey rsa:3072 \
  -keyout /opt/remotefriend/key.pem \
  -out /opt/remotefriend/cert.pem \
  -days 825 -nodes -subj "/CN=remotefriend-relay"
sudo chown root:remotefriend /opt/remotefriend/key.pem /opt/remotefriend/cert.pem
sudo chmod 0640 /opt/remotefriend/key.pem
sudo chmod 0644 /opt/remotefriend/cert.pem
```

Host ve native clientta pinlenecek tam SHA-256 fingerprint:

```bash
openssl x509 -in /opt/remotefriend/cert.pem -outform der | sha256sum | awk '{print $1}'
```

Sertifika değiştiğinde fingerprint'i güvenli bir kanaldan yeniden dağıt.

## 3. systemd

```bash
sudo cp deploy/remotefriend.service /etc/systemd/system/
sudo systemctl daemon-reload
sudo systemctl enable --now remotefriend
sudo journalctl -u remotefriend -f
```

Servis web arayüzünü yalnızca `127.0.0.1:8080` üzerinde açar. Host registry `/var/lib/remotefriend/hosts.json` içinde `0600` izinle tutulur.

## 4. Nginx + geçerli HTTPS

Tarayıcıda WebCodecs H.264 kullanabilmek ve parola/input trafiğini ağ üzerinde korumak için geçerli HTTPS gerekir.

```nginx
server {
    listen 443 ssl http2;
    server_name remote.example.com;

    ssl_certificate /etc/letsencrypt/live/remote.example.com/fullchain.pem;
    ssl_certificate_key /etc/letsencrypt/live/remote.example.com/privkey.pem;

    add_header Strict-Transport-Security "max-age=31536000" always;

    location / {
        proxy_pass http://127.0.0.1:8080;
        proxy_http_version 1.1;
        proxy_set_header Host $host;
        proxy_set_header X-Forwarded-Proto https;
        proxy_set_header Upgrade $http_upgrade;
        proxy_set_header Connection "upgrade";
        proxy_read_timeout 3600s;
        proxy_send_timeout 3600s;
    }
}
```

Let's Encrypt örneği:

```bash
sudo apt-get install -y nginx certbot python3-certbot-nginx
sudo certbot --nginx -d remote.example.com
```

## 5. Firewall

```bash
sudo ufw allow 443/tcp comment 'RemoteFriend HTTPS web'
sudo ufw allow 33202/tcp comment 'RemoteFriend TLS relay'
sudo ufw deny 8080/tcp
```

Cloud firewall/security group üzerinde de yalnızca `443` ve `33202` aç.

## 6. Host ve client ayarları

Host:

```text
RF_RV_SERVER=remote.example.com:33202
RF_RV_FP=<2. adımda alınan tam fingerprint>
```

Native clientta sunucu alanına aynı adresi gir ve `RF_RV_FP` ile aynı fingerprint'i kullan. Tarayıcıda `https://remote.example.com` aç.

## Bakım

- `/var/lib/remotefriend/hosts.json` dosyasını şifreli yedekle; içinde host kimlik sırları bulunur.
- Bir host `host_secret` dosyasını kaybederse registry'deki ilgili ID kaydı yönetici tarafından kaldırılmadan aynı ID yeniden sahiplenilemez.
- Logları, disk kullanımını, TLS sertifika süresini ve başarısız bağlantı denemelerini izle.
- `RF_PLAIN_OK=1` değerini üretim sunucusunda kullanma.
