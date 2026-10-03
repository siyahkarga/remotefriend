# Windows kurulumu

## Hazır binary ile

`remote-friend-host.exe`, `remote-friend-client.exe` ve gerekiyorsa `start_remotefriend_win64.bat` dosyalarını aynı klasöre koy.

Firewall'u yalnızca gerekli ağ profillerinde aç. Yönetici PowerShell:

```powershell
netsh advfirewall firewall add rule name="RemoteFriend Native LAN" dir=in action=allow protocol=TCP localport=33200 profile=private
netsh advfirewall firewall add rule name="RemoteFriend Web LAN" dir=in action=allow protocol=TCP localport=33201 profile=private
```

`33200` ve `33201` portlarını modemde internete yönlendirme.

`start_remotefriend_win64.bat` çalıştır (yeni sürüm çıkınca kendini günceller). `REMOTE_FRIEND_PASS` önceden ayarlanmamışsa host terminalde `abcde-23456` biçiminde bir şifre gösterir. İstemcide bu şifreyi kullan ve host terminalinde bağlantıyı `E` ile onayla.

Telefondan/tarayıcıdan en akıcı görüntü VPS'in HTTPS adresinden gelir (H.264). LAN tarayıcı sayfası (`http://IP:33201`) düz HTTP olduğu için JPEG moduna düşer; LAN'da akıcı görüntü için `remote-friend-client.exe` kullan.

## Kalite

Bağlıyken tarayıcıda ⚙ → Hızlı / Dengeli / Net. Varsayılanı değiştirmek için:

```bat
set RF_QUALITY=fast
start_remotefriend_win64.bat
```

Kalıcı bir parola kullanılacaksa en az 16 rastgele karakter seç:

```bat
set REMOTE_FRIEND_PASS=buraya_guclu_rastgele_sifre
start_remotefriend_win64.bat
```

Komut geçmişi ve `.bat` dosyası içinde gerçek parolayı bırakmamaya dikkat et.
