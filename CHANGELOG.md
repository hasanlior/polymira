# Changelog

All notable changes to POLYMIRA. Release notes are also shown in the app (Settings → About → What's new) and on the [website](https://hasanlior.github.io/polymira/changelog.html).

## 0.1.2

### New
- **Store redesign**: new full-page store layout with wide banners, screenshots, version details, and animated inline install flow for apps, emulators, themes, and mods.
- **Interface styles & themes**: new layout styles, themes, silent launcher starts, PS button support over HID, and PS3/PS4 compatibility lists.
- **POLYMIRA Orbis (experimental PS4/PS5 core)**: built-in Rust emulation core with Vulkan renderer, RDNA2 shader decoder & GPU vertex offload, GFX10 texture untiling, AudioOut2, save data mounting, and live controller/keyboard input. Decrypted PS5 game folders are recognised by the scanner and launch directly with Orbis.

### Fixed
- App icons in Store now fetch crisp website icons and Open Graph banners reliably.
- Added English translations for all new Store and Orbis UI strings.

## 0.1.1

### New
- **Search games**: Home → Search games, with the on-screen keyboard. Matches title, platform, genre and developer.
- **Pick a game for me**: Home → Pick a game for me opens a random game from your library, unplayed games first.
- **What's new**: after an update a notification points to the release notes, available any time in Settings → About → What's new.
- **Borderless full screen in emulators**: RetroArch, DuckStation, PCSX2 and Dolphin are set to borderless full screen so the in-game menu can appear over them. Can be turned off in Settings → Controller Settings.

### Fixed
- English interface: welcome wizard text, play times ("6 h 0 min"), dates and weekday names, percentages and several settings descriptions were still in Turkish.
- In-game menu showed "Resume game" twice while a game was paused; the second item now reads "Unpause game".
- Texts built from several parts (for example "PlayStation 2 · 6 h 0 min") are now translated as a whole.

## 0.1.0

First public release.

- PS3-style interface with full controller support and per-controller button symbols
- Library scan for Steam, Epic, Xbox app, GOG, Riot, Ubisoft, EA and Battle.net games, emulators and ROM folders
- One-click emulator downloads with automatic controller setup
- In-game menu on the Guide button: pause, screenshot, quick save, performance
- Trophies (Steam and RetroAchievements), notifications, play statistics, game info pages
- Save backups for every emulator with an optional sync folder
- Built-in browser, YouTube and Twitch, full-screen store for apps, themes and mods
- English and Turkish interface
