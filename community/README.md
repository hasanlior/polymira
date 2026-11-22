# Community store / Topluluk mağazası

`catalog.json` is the store catalog the app reads (when `repo` is set in `src/shared/brand.ts`).
Anyone can add a theme, a mod or an app with a pull request.

`catalog.json`, uygulamanın okuduğu mağaza kataloğudur (`src/shared/brand.ts` içinde `repo` ayarlıysa).
Herkes pull request ile tema, mod veya uygulama ekleyebilir.

## Item types / Öğe türleri

```json
{ "id": "my-theme", "kind": "theme", "name": "…", "short": "MT", "hue": 200, "author": "…", "description": "…",
  "theme": { "id": "my-theme", "name": "…", "hue": 200, "night": false, "css": ":root { … }" } }

{ "id": "my-mod", "kind": "mod", "name": "…", "short": "MM", "hue": 30, "author": "…", "description": "…",
  "mod": { "css": "mods/my-mod/style.css", "version": "1.0.0" } }

{ "id": "my-app", "kind": "app", "name": "…", "short": "AP", "hue": 120, "description": "…",
  "icon": "https://…/icon.png", "app": { "url": "https://…", "kiosk": true } }
```

## Rules / Kurallar

- `id`: letters, numbers, `-` and `_` only.
- Mods are CSS only; they cannot run code. Keep a mod under 512 KB.
- Mod CSS paths are relative to `catalog.json` (put files in `mods/<id>/`), or a full `https://` URL.
- No copyrighted logos, games, BIOS or firmware.

- `id`: yalnızca harf, rakam, `-` ve `_`.
- Modlar yalnızca CSS'tir, kod çalıştıramaz. 512 KB'ı geçmesin.
- Mod CSS yolu `catalog.json`'a göre görelidir (`mods/<id>/` klasörüne koy) ya da tam bir `https://` adresidir.
- Telifli logo, oyun, BIOS veya firmware eklenmez.

## License of contributions / Katkıların lisansı

By sending a pull request you agree that your contribution is published as part of POLYMIRA under its license (PolyForm Strict 1.0.0).

Pull request gönderen herkes katkısının POLYMIRA'nın bir parçası olarak, projenin lisansıyla (PolyForm Strict 1.0.0) yayımlanmasını kabul eder.
