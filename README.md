# XPORT

**One PC. Every Console. One Gaming Experience.**

XPORT, bilgisayarı oyun konsoluna çeviren, tamamen kontrolcüyle kullanılan bir oyun arayüzüdür. Klasik konsol menülerinden (XMB tarzı yatay/dikey menü) ilham alır; farklı platformlardaki oyunlarını tek yerde toplar ve uygun emülatörle başlatır. Oyun sırasında kontrolcünün **Guide (Xbox / PS) tuşuna** basınca konsoldaki gibi bir oyun içi menü açılır.

> XPORT oyun, BIOS, firmware veya anahtar **içermez ve dağıtmaz**. Yalnızca sahip olduğunuz yazılımların yedeklerini kullanın.

## Özellikler (v0.1)

- **XMB tarzı arayüz:** yatay kategori çubuğu, dikey alt menüler, iç içe klasörler, hareketli dalga arka planı (WebGL), seçili öğede büyüme ve parlama, menü sesleri.
- **Tam kontrolcü desteği:** Xbox, DualShock 4, DualSense ve standart gamepad'ler. Kontrolcü takılınca / çıkınca bildirim. Klavye ve fare tekerleği de çalışır.
- **Oyun kütüphanesi:** platforma göre listeler, kapak galerisi görünümü, oyun detay ekranı, favoriler, son oynananlar, son eklenenler, "Keşfet".
- **Oyun ekleme:** klasör tarama (platform seçerek ya da otomatik algılayarak), tek dosya ekleme, tekrar eklemeyi engelleme, yanlış platformdaki oyunu taşıma, `.cue/.bin` ve `.m3u` çoklu disk birleştirme. Dosyalar yerinde kalır, kopyalanmaz.
- **PS3 / PS4 / Vita oyun klasörleri:** `PARAM.SFO` okunarak gerçek oyun adı ve Title ID alınır.
- **Otomatik kapak:** [libretro-thumbnails](https://github.com/libretro-thumbnails) arşivinden (API anahtarı gerekmez). Elle kapak seçmek de mümkün.
- **Emülatör motoru:** her platform için çalıştırma profili, emülatörleri otomatik bulma, RetroArch çekirdeği kontrolü, başlamadan önce eksikleri gösterme.
- **Oyun içi menü:** Guide tuşu, Back + Start ya da `Ctrl + Shift + Home` ile açılır.
  - Oyuna devam et / gerçek duraklatma (oyun işlemi askıya alınır, emülatörden bağımsız)
  - Ekran görüntüsü al
  - Ana menüye dön (oyun duraklatılmış halde XMB'ye dönülür, "Oyuna dön" ile geri gelinir)
  - Oyunu kapat
  - Performans: oyunun CPU / RAM kullanımı, sistem CPU / RAM, NVIDIA kartlarda GPU / VRAM
- **Gerçek uyumluluk durumu:** platform seviyesi (Kararlı / Deneysel / Desteklenmiyor) ve oyunun son açılış sonucu. Çalışmayan platformlar çalışıyormuş gibi gösterilmez.
- **Oynama süresi takibi**, açılma sayısı, son oynama tarihi.
- **Ayarlar:** tam ekran / kenarlıksız / pencere, temalar, animasyon kapatma, menü sesleri, kategori gizleme, emülatör yolları, platform başına emülatör seçimi.

## Desteklenen platformlar

| Aile | Platformlar | Emülatör |
| --- | --- | --- |
| PlayStation | PS1, PS2, PSP · PS3, Vita, PS4 (deneysel) · PS5 (yok) | DuckStation, PCSX2, PPSSPP, RPCS3, Vita3K, shadPS4 |
| Xbox | Xbox, Xbox 360 (deneysel) · One, Series (yok) | xemu, Xenia Canary |
| Nintendo | NES, SNES, N64, GB, GBC, GBA, DS, 3DS, GameCube, Wii, Wii U, Switch | RetroArch, melonDS, Azahar, Dolphin, Cemu, Ryujinx |
| Sega | Master System, Mega Drive, Saturn, Dreamcast, Game Gear | RetroArch, Flycast |
| Diğer | Arcade, Neo Geo, Atari 2600/7800/Lynx, MSX, Commodore 64 | RetroArch |
| PC | `.exe`, `.lnk`, `.url` | — |

Emülatörleri kendiniz kurmanız gerekir. XPORT onları **Ayarlar → Emülasyon Ayarları → Emülatörleri otomatik bul** ile bulmaya çalışır ya da yolunu elle seçebilirsiniz.

## Kontroller

| Tuş | İşlev |
| --- | --- |
| D-Pad / sol analog | Gezinme |
| A / ✕ | Seç |
| B / ○ | Geri |
| Y / △ | Seçenekler |
| X / □ | Galeri görünümü |
| LB / RB | Kategori değiştir |
| LT / RT | Hızlı kaydır |
| Guide veya Back + Start | Oyun içi menü |

Klavye: oklar, `Enter`, `Esc`, `T` (seçenekler), `G` (galeri), `Q` / `E` (kategori), oyun içinde `Ctrl + Shift + Home`.

## Geliştirme

Gereksinimler: Windows 10/11, Node.js 20+.

```bash
npm install
npm run dev        # geliştirme modu
npm run typecheck  # tip kontrolü
npm run build      # derleme
npm run dist       # Windows kurulum dosyası (release/ klasörüne)
```

Geliştirirken pencere modunda açmak için: `npm run dev -- -- --windowed`

### Proje yapısı

```
src/
  shared/         Tipler, platform/emülatör kayıtları, IPC sözleşmesi, marka adı
  main/           Electron ana süreç
    index.ts        Pencereler, oyun içi menü, IPC
    scanner.ts      Klasör tarama ve platform tahmini
    library.ts      Kütüphane işlemleri
    emulators.ts    Emülatör bulma ve komut satırı oluşturma
    session.ts      Çalışan oyun, duraklatma, performans
    covers.ts       Otomatik kapak indirme
    gamepadWatcher.ts  Oyun sırasında Guide tuşunu dinleme (XInput)
    native/win32.ts    Win32 bağlamaları (koffi)
  preload/        Güvenli köprü (contextBridge)
  renderer/       Arayüz (React)
    src/components/  XMB, detay, galeri, dosya seçici, diyaloglar
    src/overlay/     Oyun içi menü
```

### Yeni platform / emülatör eklemek

Sadece `src/shared/platforms.ts` dosyasını düzenleyin: `EMULATORS` listesine emülatörü (exe adları ve komut satırı şablonu), `PLATFORMS` listesine platformu (uzantılar, çalıştırıcılar) ekleyin. Arayüz ve tarama otomatik güncellenir.

### Uygulamanın adını değiştirmek

`src/shared/brand.ts` içindeki değerleri ve `package.json` içindeki `name`, `productName`, `build.productName`, `build.appId` alanlarını değiştirin.

## Bilinen sınırlamalar

- Oyun içi menü, oyun **kenarlıksız tam ekran** çalışırken görünür. Özel (exclusive) tam ekran modundaki oyunların üstünde görünmeyebilir; emülatörde kenarlıksız tam ekranı seçin.
- Oyun sırasında Guide tuşu XInput üzerinden okunur. PlayStation kontrolcüleri için şimdilik Steam Input veya DS4Windows gerekir ya da `Ctrl + Shift + Home` kullanılabilir.
- Mağaza başlatıcılarına giden `.url` kısayollarında oynama süresi takip edilemez.
- Oyun bilgileri (yıl, tür, geliştirici) için çevrimiçi veritabanı henüz bağlı değil.

## Yol haritası

- Kayıt dosyası / save state yönetimi ve yedekleme
- Kontrolcü tuş özelleştirme, PlayStation kontrolcüleri için doğrudan HID desteği
- Kullanıcı profilleri, ilk açılış kurulum sihirbazı
- Çevrimiçi oyun bilgisi (açıklama, yıl, tür)
- Medya merkezi (fotoğraf, video, müzik)
- Steam / diğer mağaza kütüphanelerini içe aktarma

## Lisans

MIT — bkz. [LICENSE](LICENSE).

Bu proje Sony, Microsoft, Nintendo veya Sega ile bağlantılı değildir. Platform adları sahiplerinin ticari markalarıdır ve yalnızca uyumluluğu belirtmek için kullanılır.
