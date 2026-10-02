# Windows kurulumu

## Hazır binary ile

`remote-friend-host.exe`, `remote-friend-client.exe` ve gerekiyorsa `start_remotefriend_win64.bat` dosyalarını aynı klasöre koy.

Firewall'u yalnızca gerekli ağ profillerinde aç. Yönetici PowerShell:

```powershell
netsh advfirewall firewall add rule name="RemoteFriend Native LAN" dir=in action=allow protocol=TCP localport=33200 profile=private
netsh advfirewall firewall add rule name="RemoteFriend Web LAN" dir=in action=allow protocol=TCP localport=33201 profile=private
```

`33200` ve `33201` portlarını modemde internete yönlendirme.

`start_remotefriend_win64.bat` çalıştır. `REMOTE_FRIEND_PASS` önceden ayarlanmamışsa host terminalde rastgele bir şifre gösterecek. İstemcide bu şifreyi kullan ve host terminalinde bağlantıyı `E` ile onayla.

Akıcı görüntü için `remote-friend-client.exe` önerilir. LAN tarayıcı sayfası düz HTTP nedeniyle JPEG moduna düşebilir.

## Dengeli ayar

```bat
set RF_FPS=30
set RF_MAX_WIDTH=1600
set RF_BITRATE_BPS=6000000
start_remotefriend_win64.bat
```

Kalıcı bir parola kullanılacaksa en az 16 rastgele karakter seç:

```bat
set REMOTE_FRIEND_PASS=buraya_guclu_rastgele_sifre
start_remotefriend_win64.bat
```

Komut geçmişi ve `.bat` dosyası içinde gerçek parolayı bırakmamaya dikkat et.
