# Windows Laptop Kurulumu (5 dk)

## 1. Rust kur
https://rustup.rs → `rustup-init.exe` çalıştır, VS Build Tools isterse kur.

## 2. Klasörü kopyala
Bu `RemoteFriend/` klasörünün tamamını USB / zip / `git` ile laptopa kopyala.
Örn: `C:\RemoteFriend\`

## 3. Build al
```powershell
cd C:\RemoteFriend
cargo build --release
# çıktılar:
# target\release\remote-friend-host.exe  (~6-8 MB)
# target\release\remote-friend-client.exe (~20 MB)
```

## 4. Firewall izin (host çalışacak PC'de, Admin PowerShell)
```powershell
netsh advfirewall firewall add rule name="RemoteFriend" dir=in action=allow protocol=TCP localport=33200
```

## 5. Çalıştır
Host olacak laptop (ekranı paylaşılan):
```powershell
$env:REMOTE_FRIEND_PASS="1234"
.\target\release\remote-friend-host.exe
# IP'yi öğren: ipconfig  (örn 192.168.178.77)
```

İzleyecek PC:
```powershell
.\target\release\remote-friend-client.exe 192.168.178.77:33200 1234
```

## Test planı senin için
1. Laptopu ağa bağla, IP'yi al (`ipconfig`)
2. Laptop = host çalıştır, bu Linux = client ile bağlan:
   `./target/release/remote-friend-client 192.168.178.XX:33200 1234`
3. Görüntü geliyorsa: tıkla → laptopta mouse oynar, yazı yaz → harfler gider, "Dosya Gönder" → laptop `%TEMP%` veya `/tmp` altına `rf_*` düşer.
4. Tersini de dene (Linux host için Xorg gerekli).

Sorun olursa host logunu + client logunu at.
