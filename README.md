# POLYMIRA

**One PC. Every Console. One Gaming Experience.**

POLYMIRA, bilgisayarı oyun konsoluna çeviren, tamamen kontrolcüyle kullanılan bir oyun arayüzüdür. Klasik konsol menülerinden (XMB tarzı yatay/dikey menü) ilham alır; farklı platformlardaki oyunlarını tek yerde toplar ve uygun emülatörle başlatır. Oyun sırasında kontrolcünün **Guide (Xbox / PS) tuşuna** basınca konsoldaki gibi bir oyun içi menü açılır.

> POLYMIRA oyun, BIOS, firmware veya anahtar **içermez ve dağıtmaz**. Yalnızca sahip olduğunuz yazılımların yedeklerini kullanın.

## Özellikler

- **XMB tarzı arayüz:** yatay kategori çubuğu, dikey alt menüler, iç içe klasörler, hareketli dalga arka planı (WebGL), seçili öğede büyüme ve parlama, menü sesleri.
- **Tam kontrolcü desteği:** Xbox, DualShock 4, DualSense ve standart gamepad'ler. Kontrolcü takılınca / çıkınca bildirim. Klavye ve fare tekerleği de çalışır.
- **Oyun kütüphanesi:** platforma göre listeler, kapak galerisi görünümü, oyun detay ekranı, favoriler, son oynananlar, son eklenenler, "Keşfet".
- **Oyun ekleme:** klasör tarama (platform seçerek ya da otomatik algılayarak), tek dosya ekleme, tekrar eklemeyi engelleme, yanlış platformdaki oyunu taşıma, `.cue/.bin` ve `.m3u` çoklu disk birleştirme. Dosyalar yerinde kalır, kopyalanmaz.
- **PS3 / PS4 / Vita oyun klasörleri:** `PARAM.SFO` okunarak gerçek oyun adı ve Title ID alınır.
- **İlk açılış kurulumu:** tam ekran bir karşılama sihirbazı. Kontrolcü kontrolü, bilgisayarı tarama, bulunanları seçerek ekleme ve oyun modu ayarlarından oluşur.
- **Bilgisayarı tarama:** kurulu oyunlar, oyunun hangi klasöre ya da sürücüye kurulduğuna bakmadan bulunur:
  - **Steam:** tüm kütüphane klasörleri (`libraryfolders.vdf`)
  - **Epic Games:** eski ve yeni launcher kayıtları ve her sürücüdeki `.egstore` klasörleri
  - **Xbox uygulaması / PC Game Pass:** sürücülerdeki `.GamingRoot` kayıtları
  - **GOG**, **Riot Games** (LoL, TFT, VALORANT) ve **Ubisoft Connect**
  - **EA app, Battle.net, Rockstar ve diğer tüm kurulu oyunlar:** Windows'un yüklü uygulamalar listesi, oyun yayıncısı ve oyun motoru izleriyle (Unity, Unreal, Steamworks, EOS…) taranır
  - Kurulu emülatörler, ROM/ISO klasörleri (Masaüstü, İndirilenler, Belgeler ve tüm sürücülerde adı oyun/ROM/emülatör/platform içeren klasörler)
- **Mağaza oyunlarının takibi:** launcher üzerinden açılan oyunun işlemi kurulum klasöründen bulunur. Böylece oyun içi menü, duraklatma, oyun modu ve süre takibi bu oyunlarda da çalışır.
- **Otomatik görseller:** kapak ve tam ekran arka plan.
  - Konsol oyunlarında kendi dosyalarından (PS3 `ICON0/PIC1`, PS4/Vita `sce_sys`)
  - Steam oyunlarında Steam'den, diğer PC oyunlarında Steam'de isim eşleşmesiyle
  - Epic oyunlarında launcher'ın kendi görsel önbelleğinden
  - Retro oyunlarda [libretro-thumbnails](https://github.com/libretro-thumbnails) arşivinden (kapak ve oyun içi görüntü)
  - API anahtarı gerekmez. Elle kapak veya arka plan seçmek de mümkün.
- **PS3 tarzı oyun atmosferi:** bir oyunun üstüne gelince görseli arka plana yumuşak geçişle gelir. Biraz beklenince oyunun **resmi fragmanı** (Steam) sesiyle arka planda oynar. Steam'den alınmış **soundtrack**'ler tema müziği olarak otomatik bağlanır, her oyuna elle müzik de atanabilir.
- **Emülatör motoru:** her platform için çalıştırma profili, emülatörleri otomatik bulma, RetroArch çekirdeği kontrolü, başlamadan önce eksikleri gösterme.
- **Oyun içi menü:** Guide tuşu, Back + Start ya da `Ctrl + Shift + Home` ile açılır.
  - Oyuna devam et / gerçek duraklatma (oyun işlemi askıya alınır, emülatörden bağımsız)
  - Ekran görüntüsü al
  - Ana menüye dön (oyun duraklatılmış halde XMB'ye dönülür, "Oyuna dön" ile geri gelinir)
  - Oyunu kapat
  - Performans: oyunun CPU / RAM kullanımı, sistem CPU / RAM, NVIDIA kartlarda GPU / VRAM
- **Gerçek uyumluluk durumu:** platform seviyesi (Kararlı / Deneysel / Desteklenmiyor) ve oyunun son açılış sonucu. Çalışmayan platformlar çalışıyormuş gibi gösterilmez.
- **Oynama süresi takibi**, açılma sayısı, son oynama tarihi.
- **Otomatik emülatör indirme:** kurulum sihirbazında veya Ayarlar → Emülasyon'da istenen emülatörler resmi kaynaklarından indirilip kurulur. RetroArch için gerekli çekirdekler de iner. İndirme arka planda sürer; yüzde, hız ve kalan süre üst köşede görünür, bu sırada gezinilebilir ve oyun oynanabilir. BIOS/firmware indirilmez.
- **Uygulamalar ve Mağaza:**
  - Uygulamalar: YouTube (TV arayüzü), Netflix, Disney+, Prime Video, Spotify, Twitch ve konsolun kendi tarayıcısı
  - Tarayıcı, YouTube ve Twitch konsolun içinde açılır. Tarayıcıda sol çubuk imleç, sağ çubuk kaydırma, ✕ tıklama, ○ geri; sanal klavye (□ / Start) ile adres ve metin girilir. Her uygulamanın oturumu kalıcıdır.
  - DRM korumalı servisler (Netflix, Disney+, Prime Video, Spotify) de konsolun içinde açılır: POLYMIRA, Widevine içeren castlabs Electron sürümünü kullanır. Yayın servisleri uygulamanın VMP imzalı olmasını ister; kurulum paketi `electron-builder` sırasında imzalanır (bkz. *VMP imzası*). İmza yoksa bu uygulamalar Microsoft Edge'de tam ekran açılır. Ayarlar → Oyun Modu ve Sistem → *Korumalı uygulamalar* ile seçilebilir.
  - Uygulama ikonları her uygulamanın kendi sitesinden çalışma anında alınır; kaynak kodda üçüncü taraf logosu yoktur.
  - Mağaza tam ekran bir uygulama olarak açılır: Öne Çıkanlar, Uygulamalar, Emülatörler, Temalar, Modlar ve İndirmeler bölümleri (L1/R1). Uzaktan katalog adresi (`storeCatalogUrl`) ile yeni içerik uygulama güncellemesi gerekmeden yayınlanabilir.
- **Temalar ve modlar:** `themes/*.json` tema dosyaları ve `mods/<ad>/` CSS görünüm modları (`mod.json` + `style.css`). Modlar yalnızca görünümü değiştirir, kod çalıştıramaz.
- **Konsol tarzı kapanış:** "Çık" ya da Alt+F4 ile ekran kararır. Ortada yapılan işlemler yazar (oyun kapatılıyor, Windows masaüstü açılıyor…), iş bitince uygulama kapanır.
- **Oyun modu:** oyun başlayınca uygulanır, oyun bitince her şey eski haline döner:
  - oyuna yüksek işlemci önceliği
  - Dengeli / Güç tasarrufu planındaysan Yüksek performans güç planı (zaten performans planındaysan dokunulmaz)
  - isteğe bağlı olarak Windows masaüstünü (görev çubuğu, `explorer.exe`) kapatma
- **Çökmeye dayanıklılık:** küçük bir bekçi süreci uygulamayı izler. Uygulama beklenmedik şekilde kapanırsa masaüstünü ve güç planını geri getirir, uygulamayı yeniden açar. Üst üste çökmelerde döngüye girmez. Arayüz süreci çökerse kendini yeniden yükler.
- **Windows ile başlatma** (kurulu sürümde): bilgisayar açılınca doğrudan konsol arayüzü gelir.
- **Ayarlar:** tam ekran / kenarlıksız / pencere, temalar, animasyon kapatma, menü sesleri, oyun arka planları / fragmanları / müzikleri, kategori gizleme, emülatör yolları, platform başına emülatör seçimi.

## Desteklenen platformlar

| Aile | Platformlar | Emülatör |
| --- | --- | --- |
| PlayStation | PS1, PS2, PSP · PS3, Vita, PS4 (deneysel) · PS5 (yok) | DuckStation, PCSX2, PPSSPP, RPCS3, Vita3K, shadPS4 |
| Xbox | Xbox, Xbox 360 (deneysel) · One, Series (yok) | xemu, Xenia Canary |
| Nintendo | NES, SNES, N64, GB, GBC, GBA, DS, 3DS, GameCube, Wii, Wii U, Switch | RetroArch, melonDS, Azahar, Dolphin, Cemu, Ryujinx |
| Sega | Master System, Mega Drive, Saturn, Dreamcast, Game Gear | RetroArch, Flycast |
| Diğer | Arcade, Neo Geo, Atari 2600/7800/Lynx, MSX, Commodore 64 | RetroArch |
| PC | Steam, Epic, Xbox uygulaması, GOG, Riot, Ubisoft, `.exe`, `.lnk`, `.url` | — |

Emülatörleri kendiniz kurmanız gerekir. POLYMIRA onları **Ayarlar → Emülasyon Ayarları → Emülatörleri otomatik bul** ile bulmaya çalışır ya da yolunu elle seçebilirsiniz.

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

### VMP imzası (Netflix vb. için)

Yayın servisleri Widevine'in yanında uygulamanın castlabs EVS ile imzalanmasını ister. Ücretsiz bir EVS hesabı bir kez açılır:

```bash
pip install --upgrade castlabs-evs
python -m castlabs_evs.account signup
```

Sonra `npm run dist` paketlenen uygulamayı otomatik imzalar (`scripts/vmp-sign.cjs`). Geliştirme ortamındaki Electron'u imzalamak için `npm run vmp:sign`.

### Uygulamanın adını değiştirmek

`src/shared/brand.ts` içindeki değerleri ve `package.json` içindeki `name`, `productName`, `build.productName`, `build.appId` alanlarını değiştirin.

## Bilinen sınırlamalar

- Oyun içi menü, oyun **kenarlıksız tam ekran** çalışırken görünür. Özel (exclusive) tam ekran modundaki oyunların üstünde görünmeyebilir; emülatörde kenarlıksız tam ekranı seçin.
- Oyun sırasında Guide tuşu XInput üzerinden okunur. PlayStation kontrolcüleri için şimdilik Steam Input veya DS4Windows gerekir ya da `Ctrl + Shift + Home` kullanılabilir.
- Elle eklenen `.url` kısayollarında oynama süresi takip edilemez (taranarak bulunan mağaza oyunlarında edilir).
- Fragmanlar ve Steam isim eşleşmesi yalnızca Steam mağazasında bulunan oyunlar için çalışır. Her oyunun soundtrack'ini internetten yasal olarak çekebilecek açık bir kaynak yok; tema müziği Steam soundtrack'lerinden veya elle seçilen dosyadan gelir.
- Dolphin ve Ryujinx'in sabit bir indirme adresi olmadığı için otomatik indirilemiyor; elle kurulup otomatik algılanabilir.
- Netflix gibi DRM korumalı uygulamalar konsolun içinde yalnızca VMP imzalı kurulumda oynatılır; geliştirme sürümünde (`npm run dev`) Edge kullanılır.
- PC oyunları ve emülatörler kendi pencerelerinde tam ekran çalışır; konsol arkada bekler ve oyun içi menü üstlerinde açılır.
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
