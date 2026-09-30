# Windows Laptop — kurulum yok, 2 dk

## 1. İndir
https://github.com/siyahkarga/remotefriend/releases → en yeni sürüm →
`remote-friend-host.exe` + `HOST_BASLAT.bat` indir, aynı klasöre koy.

## 2. Firewall (Admin PowerShell, bir kez)
```powershell
netsh advfirewall firewall add rule name="RemoteFriend" dir=in action=allow protocol=TCP localport=33200
netsh advfirewall firewall add rule name="RemoteFriendWeb" dir=in action=allow protocol=TCP localport=33201
```

## 3. Çalıştır
`HOST_BASLAT.bat` çift tık. Ekranda şunu görürsün:
```
Tarayıcı ile bağlan: http://192.168.X.X:33201  (aynı ağdan)
```

## 4. Bağlan (istemci taraf)
- Aynı WiFi'deki telefon/PC/tablet: tarayıcıda yukarıdaki adresi aç (Chrome/Edge).
- Şifre: `1234` → Bağlan.
- **Bu laptopta terminalde `E` tuşuna bas (onay).** Onay yoksa görüntü gitmez!

Hepsi bu. Rust kurmana, derlemene gerek yok.
