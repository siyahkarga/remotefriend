# VPS Kurulumu (Ubuntu/Debian, ~10 dk, bir kez)

## 1. Binary indir
```bash
sudo mkdir -p /opt/remotefriend && cd /opt/remotefriend
sudo curl -sL https://github.com/siyahkarga/remotefriend/releases/download/v0.5.0/remote-friend-rendezvous -o remote-friend-rendezvous
sudo chmod +x remote-friend-rendezvous
```

## 2. TLS sertifikası (self-signed, 10 yıl)
```bash
sudo openssl req -x509 -newkey rsa:2048 -keyout /opt/remotefriend/key.pem -out /opt/remotefriend/cert.pem -days 3650 -nodes -subj "/CN=remotefriend"
sudo chmod 600 /opt/remotefriend/key.pem
# fingerprint (host ve clientlara LAZIM — bir kenara yaz):
openssl x509 -in /opt/remotefriend/cert.pem -noout -fingerprint -sha256 | tr -d ':' | tr 'A-Z' 'a-z'
```

## 3. Servis olarak çalıştır
Repoda `deploy/remotefriend.service` dosyası var:
```bash
sudo cp /yol/remotefriend.service /etc/systemd/system/
sudo systemctl daemon-reload
sudo systemctl enable --now remotefriend
sudo journalctl -u remotefriend -f   # log izle, "rendezvous dinliyor" görünmeli
```

## 4. Firewall aç (VPS + varsa cloud panel)
```bash
sudo ufw allow 33201/tcp comment 'remotefriend web'
sudo ufw allow 33202/tcp comment 'remotefriend relay'
```

## 5. Host ve clientlara tanıt
- **Host PC:** `RF_RV_SERVER=VPS_IP:33202` + `RF_RV_FP=<2. adımdaki fingerprint>`
  (ya da bir kez config dosyasına yazılır, program hatırlar)
- **Client:** sunucu satırına `VPS_IP:33202` + fingerprint (ilk bağlanışta sorulur/kaydedilir)
- **Tarayıcı:** `http://VPS_IP:33201` → ID kodu gir (nginx + domain ile https önerilir)

## 6. (Önerilen) nginx + gerçek domain
```nginx
server {
  listen 443 ssl;
  server_name remote.senin-domainin.com;
  ssl_certificate /etc/letsencrypt/live/remote.senin-domainin.com/fullchain.pem;
  ssl_certificate_key /etc/letsencrypt/live/remote.senin-domainin.com/privkey.pem;
  location / {
    proxy_pass http://127.0.0.1:8080;
    proxy_http_version 1.1;
    proxy_set_header Upgrade $http_upgrade;
    proxy_set_header Connection "upgrade";
  }
}
```
Bu durumda tarayıcı `https://remote.senin-domainin.com` açar, sertifika uyarısı çıkmaz.
Native relay (33202) yine VPS'nin kendi sertifikasını kullanır (fingerprint ile).
