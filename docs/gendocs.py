# Generates the POLYMIRA documentation pages (docs/docs/*.html) from the content below.
import io, os, re, html

ROOT = 'docs/docs'
GROUPS = [
    ('Getting started', ['index', 'install', 'first-run']),
    ('Using POLYMIRA', ['library', 'controllers', 'in-game-menu', 'apps', 'store']),
    ('Emulation', ['emulators', 'saves', 'trophies']),
    ('Reference', ['settings', 'troubleshooting', 'building']),
]

P = {}

P['index'] = ('Introduction', 'What POLYMIRA is and what it does.', """
<figure><img src="../assets/shots/home.jpg" alt="POLYMIRA home screen" /><figcaption>The home screen, inside the PlayStation 2 folder.</figcaption></figure>
<p>POLYMIRA is a full-screen frontend for Windows that turns a PC into something that behaves like a game console. It collects the games you already have — store games and emulated console games — into one menu, starts them, and gives you a console-style menu on top of them while they run.</p>
<h2 id="what-it-does">What it does</h2>
<ul>
<li>Finds games from Steam, Epic Games, the Xbox app, GOG, EA, Ubisoft Connect, Battle.net and Riot, plus ROMs and disc images for emulated systems.</li>
<li>Downloads emulators from their official sources and configures their controller settings before a game starts.</li>
<li>Opens an in-game menu on the controller's Guide button: pause, screenshot, quick save, performance, quit.</li>
<li>Keeps play time, trophies and save backups for everything in the library.</li>
</ul>
<h2 id="what-it-does-not">What it does not do</h2>
<ul>
<li>It does not include or download games, BIOS files, firmware or encryption keys.</li>
<li>It does not replace Steam or other launchers. Store games still start through their own launchers.</li>
<li>It does not move, copy or modify your game files.</li>
</ul>
<h2 id="requirements">Requirements</h2>
<table><tr><th>Item</th><th>Requirement</th></tr>
<tr><td>Operating system</td><td>Windows 10 or 11, 64-bit</td></tr>
<tr><td>Disk space</td><td>About 350 MB for the app, plus space for emulators you download</td></tr>
<tr><td>Input</td><td>Any controller supported by Windows, or a keyboard</td></tr></table>
""")

P['install'] = ('Installation', 'Download, install, update and uninstall.', """
<h2 id="download">Download</h2>
<p>Get the latest version from <a href="https://github.com/hasanlior/polymira/releases/latest">GitHub Releases</a>. Two files are published:</p>
<table><tr><th>File</th><th>Use it when</th></tr>
<tr><td><code>POLYMIRA-Setup-x.y.z.exe</code></td><td>You want a normal install with a Start menu entry, a desktop shortcut and automatic updates.</td></tr>
<tr><td><code>POLYMIRA-Portable-x.y.z.exe</code></td><td>You want to run it without installing, for example from a USB drive.</td></tr></table>
<h2 id="smartscreen">SmartScreen warning</h2>
<p>The installer is not code-signed yet, so Windows may show “Windows protected your PC”. Choose <b>More info</b> and then <b>Run anyway</b>.</p>
<h2 id="updates">Updates</h2>
<p>The installed version checks GitHub Releases when it starts. A new version is downloaded in the background and verified against the SHA-256 digest GitHub publishes for the file. You get a notification when it is ready; install it from <b>Settings → About → Install update</b>. The app closes, installs and starts again.</p>
<p>Automatic checks can be turned off with <b>Settings → About → Check for updates on start</b>.</p>
<h2 id="data">Where your data lives</h2>
<p>Settings, the library, covers, screenshots and save backups are stored in <code>%APPDATA%\\POLYMIRA</code>. Uninstalling the app keeps this folder; delete it yourself for a clean start.</p>
<h2 id="uninstall">Uninstall</h2>
<p>Use <b>Settings → Apps → Installed apps</b> in Windows and remove POLYMIRA. Emulators downloaded by POLYMIRA live inside the data folder above.</p>
""")

P['first-run'] = ('First run', 'The welcome wizard, step by step.', """
<figure><img src="../assets/shots/setup.jpg" alt="Welcome screen" /><figcaption>The first screen of the welcome wizard.</figcaption></figure>
<p>The wizard runs full screen the first time you open POLYMIRA. You can run it again later from <b>Settings → Game Mode &amp; System → Run the setup wizard again</b>.</p>
<ol>
<li><b>Controller.</b> Connect a controller and press any button. A keyboard works too (<kbd>Enter</kbd>).</li>
<li><b>Scan.</b> POLYMIRA reads the Steam, Epic, GOG, Xbox app, Riot and Ubisoft libraries, looks through installed programs for other games, finds installed emulators and scans folders whose names suggest games or ROMs on every drive.</li>
<li><b>Results.</b> Found games are grouped by source. Untick anything you don't want and choose <b>Add selected</b>.</li>
<li><b>Emulators.</b> Emulators needed for the games you added are preselected for download. Downloads run in the background.</li>
<li><b>Game mode.</b> Choose what happens while you play: higher CPU priority, a performance power plan, and optionally closing the Windows desktop.</li>
</ol>
<div class="note"><p>Nothing is copied or moved during the scan. Games stay where they are; POLYMIRA only remembers their paths.</p></div>
""")

P['library'] = ('Library', 'How games are found, organised and launched.', """
<figure><img src="../assets/shots/detail.jpg" alt="Game detail page" /><figcaption>A game's detail page: description, details, play time and actions.</figcaption></figure>
<h2 id="categories">Categories</h2>
<p>The top row holds the categories: Home, PlayStation, Xbox, Nintendo, Sega, Other Platforms, PC Games, Apps, Media, Store and Settings. Console categories contain one folder per platform; categories without games stay hidden.</p>
<h2 id="store-games">Store games</h2>
<p>Games from Steam, Epic, the Xbox app, GOG, EA, Ubisoft Connect, Battle.net and Riot start through their own launchers. POLYMIRA then finds the game's process from its install folder, so play time, the in-game menu and game mode work for them too.</p>
<h2 id="adding">Adding games yourself</h2>
<ul>
<li><b>Home → Add games → Add game folder</b> scans a folder for one platform, or detects the platform from file types and folder names.</li>
<li><b>Add a single game</b> adds one file. For PC games you can pick an <code>.exe</code>, a shortcut (<code>.lnk</code>) or an internet shortcut (<code>.url</code>).</li>
<li>Multi-disc games are grouped through <code>.cue</code>/<code>.bin</code> and <code>.m3u</code> files. PS3, PS4 and Vita game folders are read from their <code>PARAM.SFO</code>.</li>
</ul>
<p>Added folders are watched: <b>Settings → Library &amp; Storage → Rescan all folders</b> picks up new games, and scanning on start can be turned on there.</p>
<h2 id="artwork">Artwork and information</h2>
<p>Covers and backgrounds come from the game files themselves (PS3/PS4/Vita), Steam, the Epic launcher's cache and the libretro thumbnail archive. Descriptions, year, genre and developer come from the Steam store, the libretro database and Wikipedia. You can set your own cover, background and theme music from a game's options.</p>
<h2 id="gallery">Gallery view</h2>
<p>Press <kbd>X</kbd> / <kbd>□</kbd> on a platform folder to see its games as a grid of covers.</p>
<figure><img src="../assets/shots/gallery.jpg" alt="Gallery view" /></figure>
<h2 id="compatibility">Compatibility labels</h2>
<p>Each platform is marked <b>Stable</b>, <b>Experimental</b> or <b>Unsupported</b>, and each game shows the result of its last launch. A platform with no working emulator is never shown as playable.</p>
""")

P['controllers'] = ('Controllers', 'Supported controllers, button symbols and remapping.', """
<h2 id="supported">Supported controllers</h2>
<p>Xbox controllers, DualShock 4, DualSense, Switch Pro and most controllers that Windows recognises as gamepads. The keyboard and mouse wheel also work.</p>
<h2 id="default">Default buttons</h2>
<table><tr><th>Xbox</th><th>PlayStation</th><th>Keyboard</th><th>Action</th></tr>
<tr><td>D-pad / left stick</td><td>D-pad / left stick</td><td>Arrows</td><td>Move</td></tr>
<tr><td>A</td><td>✕</td><td><kbd>Enter</kbd></td><td>Select</td></tr>
<tr><td>B</td><td>○</td><td><kbd>Esc</kbd></td><td>Back</td></tr>
<tr><td>Y</td><td>△</td><td><kbd>T</kbd></td><td>Options</td></tr>
<tr><td>X</td><td>□</td><td><kbd>G</kbd></td><td>Gallery</td></tr>
<tr><td>LB / RB</td><td>L1 / R1</td><td><kbd>Q</kbd> / <kbd>E</kbd></td><td>Previous / next category</td></tr>
<tr><td>Guide, or Back + Start</td><td>PS, or Share + Options</td><td><kbd>Ctrl</kbd>+<kbd>Shift</kbd>+<kbd>Home</kbd></td><td>In-game menu</td></tr></table>
<h2 id="symbols">Button symbols</h2>
<p>The hints at the bottom of the screen follow the controller you last used: A B X Y for Xbox, ✕ ○ □ △ for PlayStation, the Nintendo layout for Switch pads, key names for the keyboard. To always show one style, use <b>Settings → Controller Settings → Button symbols</b>.</p>
<h2 id="remap">Remapping</h2>
<figure><img src="../assets/shots/remap.jpg" alt="Button mapping screen" /></figure>
<p>Open <b>Settings → Controller Settings → Button mapping</b>, select an action and press the button you want for it. If that button was used by another action, the two are swapped. <b>Restore default buttons</b> resets everything. The D-pad always stays as navigation.</p>
<h2 id="emulators">Controllers in emulators</h2>
<p>See <a href="emulators.html#controllers">Emulators → Controller setup</a>.</p>
""")

P['in-game-menu'] = ('In-game menu', 'The menu you open on top of a running game.', """
<figure><img src="../assets/shots/ingame.jpg" alt="In-game menu" /></figure>
<p>While a game runs, press the controller's <b>Guide</b> button (or <b>Back + Start</b>, or <kbd>Ctrl</kbd>+<kbd>Shift</kbd>+<kbd>Home</kbd>). Which of these opens the menu is set in <b>Settings → Controller Settings → In-game menu button</b>.</p>
<table><tr><th>Item</th><th>What it does</th></tr>
<tr><td>Resume game</td><td>Closes the menu and returns to the game.</td></tr>
<tr><td>Pause / Unpause game</td><td>Suspends the game process itself, independent of the emulator.</td></tr>
<tr><td>Take screenshot</td><td>Saves the screen; screenshots are listed under <b>Media → Screenshots</b>.</td></tr>
<tr><td>Quick save / Quick load</td><td>Save and load a state in RetroArch games.</td></tr>
<tr><td>Return to home menu</td><td>Shows the POLYMIRA home screen with the game paused in the background.</td></tr>
<tr><td>Quit game</td><td>Closes the game and everything it started.</td></tr></table>
<p>The performance panel shows the game's CPU and RAM use, system CPU and RAM, and GPU usage on NVIDIA cards.</p>
<h2 id="borderless">Exclusive full screen</h2>
<div class="note warn"><p>The menu draws over games running in <b>borderless</b> full screen. Games in <b>exclusive</b> full screen may hide it. Choose borderless or windowed full screen in the game's display settings.</p></div>
<h2 id="behind">While you play</h2>
<p>POLYMIRA stays open behind the game on a black screen and stops its own animation, so it does not use GPU time. When the game exits, you are back on the home screen.</p>
""")

P['apps'] = ('Apps and browser', 'YouTube, Twitch, streaming services and the built-in browser.', """
<h2 id="inside">Apps that run inside the console</h2>
<p>The built-in browser, YouTube (TV interface) and Twitch open inside POLYMIRA. Each app keeps its own login.</p>
<table><tr><th>Control</th><th>Browser</th><th>YouTube / Twitch</th></tr>
<tr><td>Left stick</td><td>Pointer</td><td>Move</td></tr>
<tr><td>Right stick</td><td>Scroll</td><td>—</td></tr>
<tr><td>A / ✕</td><td>Click</td><td>Select</td></tr>
<tr><td>B / ○</td><td>Back a page</td><td>Back</td></tr>
<tr><td>X / □</td><td>On-screen keyboard</td><td>On-screen keyboard</td></tr>
<tr><td>Start</td><td>Address or search</td><td>On-screen keyboard</td></tr>
<tr><td>Y / △</td><td>App menu</td><td>App menu</td></tr></table>
<p>The app menu has back, forward, reload, start page, zoom and close. With a keyboard, <kbd>F1</kbd> opens it.</p>
<h2 id="drm">Netflix, Disney+, Prime Video, Spotify</h2>
<p>These services require Widevine DRM and a signed app binary. In builds that are not VMP-signed they open full screen in Microsoft Edge, with the in-game menu available on top. Choose the behaviour in <b>Settings → Game Mode &amp; System → Protected apps</b>.</p>
<h2 id="add">Adding apps</h2>
<p>Use <b>Apps → Add apps from the store</b>, or the store's Apps tab. App icons are loaded from each service's own website.</p>
""")

P['store'] = ('Store, themes and mods', 'Install apps, emulators, themes and community mods.', """
<figure><img src="../assets/shots/store.jpg" alt="Store, Emulators tab" /></figure>
<p>Open the store from the <b>Store</b> category. Switch sections with <kbd>LB</kbd> / <kbd>RB</kbd>: Featured, Apps, Emulators, Themes, Mods and Downloads.</p>
<h2 id="themes">Themes</h2>
<p>Themes change the background colour and can add CSS. Apply one from the store, or drop a <code>.json</code> file into the themes folder (<b>Store → Mods → Theme folder</b>):</p>
<pre><code>{ "name": "My theme", "hue": 210, "night": false, "css": ":root { }" }</code></pre>
<h2 id="mods">Mods</h2>
<p>A mod is a folder in the mods folder with a <code>mod.json</code> (<code>name</code>, <code>description</code>) and a <code>style.css</code>. Mods change how the interface looks; they cannot run code. Turn them on and off in <b>Store → Mods</b>.</p>
<h2 id="community">Community catalog</h2>
<p>The store also reads <code>community/catalog.json</code> from the GitHub repository. Anyone can add a theme, mod or app with a pull request; the format is described in <a href="https://github.com/hasanlior/polymira/tree/main/community">community/README.md</a>.</p>
""")

P['emulators'] = ('Emulators', 'Downloading, configuring and choosing emulators.', """
<h2 id="download">Downloading</h2>
<p>From the store's Emulators tab or <b>Settings → Emulation Settings → Download emulators</b>. Downloads come from each project's official releases and run in the background with progress, speed and time left in the top corner.</p>
<table><tr><th>Emulator</th><th>Systems</th></tr>
<tr><td>RetroArch (with the cores your games need)</td><td>NES, SNES, N64, Game Boy family, Sega, arcade and more</td></tr>
<tr><td>DuckStation</td><td>PlayStation</td></tr>
<tr><td>PCSX2</td><td>PlayStation 2</td></tr>
<tr><td>PPSSPP</td><td>PSP</td></tr>
<tr><td>RPCS3, Vita3K, shadPS4</td><td>PS3, Vita, PS4 (experimental)</td></tr>
<tr><td>xemu, Xenia Canary</td><td>Xbox, Xbox 360 (experimental)</td></tr>
<tr><td>Cemu, melonDS, Azahar, Flycast</td><td>Wii U, DS, 3DS, Dreamcast</td></tr></table>
<p>Dolphin and Ryujinx have no stable download link; install them yourself and use <b>Find emulators automatically</b>.</p>
<h2 id="bios">BIOS and system files</h2>
<p>Some systems need files from your own console: a PS1 or PS2 BIOS, PS3 system software, Switch keys, a Saturn BIOS. POLYMIRA does not provide them. Place them where the emulator expects them; the platform's note in the app says what is needed.</p>
<h2 id="controllers">Controller setup</h2>
<p>With <b>Automatic controller setup in emulators</b> on (the default), POLYMIRA writes controller settings before a game starts:</p>
<table><tr><th>Emulator</th><th>Setup</th></tr>
<tr><td>DuckStation, PCSX2, Dolphin (GameCube), RPCS3</td><td>Written by POLYMIRA the first time, then left alone</td></tr>
<tr><td>RetroArch, PPSSPP, Vita3K, shadPS4, xemu, Xenia, Flycast</td><td>Detect controllers on their own</td></tr>
<tr><td>Cemu, Ryujinx, melonDS, Azahar</td><td>Set up once in the emulator</td></tr></table>
<p>If you change controls inside an emulator, POLYMIRA keeps your changes. <b>Re-apply controller settings</b> writes the defaults again.</p>
<h2 id="per-game">Per-game settings</h2>
<p>A game's options contain <b>Emulator settings (this game)</b>: the emulator to use, a video filter for RetroArch games (CRT, LCD grid, sharp smoothing) and extra command line arguments.</p>
""")

P['saves'] = ('Save data', 'Backups and restores for every emulator.', """
<p><b>Settings → Save Data Management</b> lists every configured emulator with the folders where it keeps saves and save states. POLYMIRA finds them in the emulator's portable folder or in Documents / AppData.</p>
<h2 id="backup">Backups</h2>
<ul>
<li>With <b>Back up automatically after playing</b> on, the emulator's saves are zipped when a play session ends.</li>
<li>The last 10 backups per emulator are kept.</li>
<li><b>Back up all saves now</b> backs up every emulator at once.</li>
</ul>
<h2 id="restore">Restore</h2>
<p>Pick an emulator, <b>Restore from backup</b>, then a date. Your current saves are backed up before the restore, so it can always be undone.</p>
<h2 id="sync">Sync folder</h2>
<p>Choose a folder inside OneDrive, Google Drive or on a USB drive. Every backup is copied there as well, and backups found there appear in the restore list on other PCs.</p>
<div class="note"><p>melonDS stores saves next to the game file; those <code>.sav</code> files are included in its backups.</p></div>
""")

P['trophies'] = ('Trophies and statistics', 'Achievements, play time and notifications.', """
<h2 id="steam">Steam achievements</h2>
<p>Read from the files the Steam client keeps on disk. No account or key is needed. A game's detail page shows its progress; the full list is under <b>Home → Trophies</b>.</p>
<h2 id="ra">RetroAchievements</h2>
<p>For emulated games, add your <a href="https://retroachievements.org">RetroAchievements</a> username and Web API key in <b>Settings → Game Mode &amp; System → RetroAchievements account</b>. The key is on your RetroAchievements settings page.</p>
<h2 id="notifications">Notifications</h2>
<p>Unlocked trophies, finished downloads, backups and updates appear as cards in the top right corner, also over a running game. Turn the in-game cards off with <b>Notifications while playing</b>. Past notifications are under <b>Home → Notifications</b>.</p>
<h2 id="stats">Play statistics</h2>
<figure><img src="../assets/shots/stats.jpg" alt="Play statistics" /></figure>
<p><b>Home → Play statistics</b> shows the last seven days, weekly and total play time, and your most played games.</p>
""")

P['settings'] = ('Settings', 'Every section of the Settings category.', """
<table><tr><th>Section</th><th>Contains</th></tr>
<tr><td>Display Settings</td><td>Window mode (full screen, borderless, window), background theme, game backgrounds</td></tr>
<tr><td>Sound Settings</td><td>Menu sounds and volume, game trailers, theme music and its volume</td></tr>
<tr><td>Controller Settings</td><td>Connected controllers, button symbols, button mapping, automatic emulator controller setup, vibration, in-game menu button</td></tr>
<tr><td>Game Mode &amp; System</td><td>Pause on menu, CPU priority, power plan, closing the desktop while playing, notifications, protected apps, RetroAchievements, restart after a crash, start with Windows, setup wizard</td></tr>
<tr><td>Emulation Settings</td><td>Find emulators, download emulators, scan an emulator folder, emulator per platform, BIOS notes</td></tr>
<tr><td>Save Data Management</td><td>Backups, restores and the sync folder</td></tr>
<tr><td>Library &amp; Storage</td><td>Game folders, rescans, cover downloads</td></tr>
<tr><td>Interface</td><td>Language (English / Türkçe), hidden categories, animations</td></tr>
<tr><td>About</td><td>Version, updates, legal information</td></tr></table>
<h2 id="game-mode">Game mode details</h2>
<ul>
<li><b>CPU priority:</b> the game process runs at high priority.</li>
<li><b>Power plan:</b> switches from Balanced or Power saver to High performance while you play. A performance plan you already use is left alone.</li>
<li><b>Close the Windows desktop:</b> stops the taskbar and Explorer during play and starts them again afterwards. Open File Explorer windows close.</li>
</ul>
<p>All of these are undone when the game ends, when you quit, and after a crash.</p>
""")

P['troubleshooting'] = ('Troubleshooting', 'Common problems and how to solve them.', """
<h3 id="t-smartscreen">Windows blocks the installer</h3>
<p>Choose <b>More info → Run anyway</b>. The installer is not code-signed yet.</p>
<h3 id="t-menu">The in-game menu does not appear</h3>
<p>Switch the game or emulator to borderless full screen. With a PlayStation controller, the PS button is not visible to Windows' Xbox controller interface: use Steam Input or DS4Windows, Back + Start, or <kbd>Ctrl</kbd>+<kbd>Shift</kbd>+<kbd>Home</kbd>.</p>
<h3 id="t-emu">“Emulator not set”</h3>
<p>Download it from the store, or use <b>Settings → Emulation Settings → Find emulators automatically</b> or <b>Scan emulator folder</b>.</p>
<h3 id="t-bios">A game starts and closes immediately</h3>
<p>The emulator is usually missing a BIOS or system files. Check the platform note in the app and the emulator's own documentation.</p>
<h3 id="t-core">“RetroArch core missing”</h3>
<p>Cores are downloaded with RetroArch. For other cores use RetroArch's <b>Online Updater → Core Downloader</b>.</p>
<h3 id="t-store">A store game opens but play time is not counted</h3>
<p>POLYMIRA finds the game's process from its install folder. Games added only as a <code>.url</code> shortcut have no folder to look in; add them through the PC scan instead.</p>
<h3 id="t-netflix">Netflix opens in Edge</h3>
<p>Expected in builds without a VMP signature. See <a href="apps.html#drm">Apps → Netflix</a>.</p>
<h3 id="t-desktop">The taskbar is missing after a crash</h3>
<p>The watchdog restores it automatically. If it does not, press <kbd>Ctrl</kbd>+<kbd>Shift</kbd>+<kbd>Esc</kbd>, choose <b>Run new task</b> and start <code>explorer.exe</code>.</p>
<h3 id="t-reset">Start over</h3>
<p>Quit POLYMIRA and delete <code>%APPDATA%\\POLYMIRA</code>. The next start runs the welcome wizard again.</p>
<h3 id="t-report">Report a problem</h3>
<p>Open an issue on <a href="https://github.com/hasanlior/polymira/issues">GitHub</a> with your Windows version, the game's platform and what happened.</p>
""")

P['building'] = ('Building from source', 'Run and package POLYMIRA yourself.', """
<p>Requirements: Windows 10 or 11 and Node.js 20 or newer.</p>
<pre><code>npm install
npm run dev        # development
npm run typecheck  # type check
npm run dist       # installer and portable exe in release/</code></pre>
<p>POLYMIRA is built with Electron (the castlabs build with Widevine), React and TypeScript. Every build places a POLYMIRA shortcut on the desktop; set <code>NO_SHORTCUT=1</code> to skip it.</p>
<h2 id="vmp">VMP signing</h2>
<p>Streaming services require the app to be VMP-signed. Create a free castlabs EVS account once, then <code>npm run dist</code> signs the packaged app:</p>
<pre><code>pip install --upgrade castlabs-evs
python -m castlabs_evs.account signup</code></pre>
<h2 id="platforms">Adding a platform or emulator</h2>
<p>Edit <code>src/shared/platforms.ts</code>: add the emulator to <code>EMULATORS</code> (executable names and command line) and the platform to <code>PLATFORMS</code> (file types and runners). The interface and scanner pick it up automatically.</p>
<h2 id="license">License</h2>
<p>The source is available under the <a href="https://github.com/hasanlior/polymira/blob/main/LICENSE">PolyForm Strict License 1.0.0</a>: you may use POLYMIRA for noncommercial purposes; redistribution and modified versions are not allowed.</p>
""")

ORDER = [p for _, ps in GROUPS for p in ps]
STAR = '<svg viewBox="0 0 24 24" fill="currentColor"><path d="m12 3.5 2.6 5.4 5.9.8-4.3 4.1 1 5.8L12 16.8l-5.2 2.8 1-5.8-4.3-4.1 5.9-.8Z"/></svg>'

def href(p):
    return 'index.html' if p == 'index' else f'{p}.html'

def page(slug):
    title, intro, body = P[slug]
    group = next(g for g, ps in GROUPS if slug in ps)
    side = ''
    for g, ps in GROUPS:
        side += f'<h5>{g}</h5>' + ''.join(f'<a href="{href(p)}"{" class=\"on\"" if p == slug else ""}>{P[p][0]}</a>' for p in ps)
    heads = re.findall(r'<h[23] id="([^"]+)">(.*?)</h[23]>', body)
    toc = ('<aside class="toc"><b>On this page</b>' + ''.join(f'<a href="#{i}">{re.sub("<[^>]+>", "", t)}</a>' for i, t in heads) + '</aside>') if heads else '<aside class="toc"></aside>'
    i = ORDER.index(slug)
    prev = f'<a class="prev" href="{href(ORDER[i-1])}"><small>Previous</small>{P[ORDER[i-1]][0]}</a>' if i > 0 else '<span></span>'
    nxt = f'<a class="next" href="{href(ORDER[i+1])}"><small>Next</small>{P[ORDER[i+1]][0]}</a>' if i < len(ORDER) - 1 else '<span></span>'
    return f"""<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8" />
  <meta name="viewport" content="width=device-width, initial-scale=1" />
  <title>{html.escape(title)} · POLYMIRA Docs</title>
  <meta name="description" content="{html.escape(intro)}" />
  <meta name="theme-color" content="#05060f" />
  <link rel="icon" href="../assets/icon.svg" type="image/svg+xml" />
  <link rel="preconnect" href="https://fonts.googleapis.com" />
  <link rel="preconnect" href="https://fonts.gstatic.com" crossorigin />
  <link href="https://fonts.googleapis.com/css2?family=Inter:wght@400;500;600;700&family=Space+Grotesk:wght@500;600;700&display=swap" rel="stylesheet" />
  <link rel="stylesheet" href="../assets/site.css" />
</head>
<body>
  <nav class="top">
    <div class="wrap">
      <a class="brand" href="../"><img src="../assets/icon.svg" alt="" />POLYMIRA</a>
      <div class="navlinks">
        <a href="../#tour">Tour</a>
        <a href="../#features">Features</a>
        <a href="./" class="on">Docs</a>
        <a href="https://github.com/hasanlior/polymira/releases">Releases</a>
      </div>
      <a class="navstar" href="https://github.com/hasanlior/polymira" title="Star POLYMIRA on GitHub">{STAR}<span>Star</span><b class="stars"></b></a>
      <a class="navcta dl" href="https://github.com/hasanlior/polymira/releases/latest">Download</a>
    </div>
  </nav>
  <div class="docs">
    <div>
      <button class="side-toggle" type="button">☰ Documentation menu</button>
      <aside class="side">{side}</aside>
    </div>
    <main class="doc">
      <div class="crumbs">Docs / {group}</div>
      <h1 class="display">{title}</h1>
      <p class="intro">{intro}</p>
{body.strip()}
      <div class="pager">{prev}{nxt}</div>
      <p class="crumbs" style="margin-top:28px"><a href="https://github.com/hasanlior/polymira/blob/main/docs/gendocs.py" style="color:var(--faint)">Edit this page on GitHub</a></p>
    </main>
    {toc}
  </div>
  <footer>
    <div class="wrap">
      <span>© 2026 POLYMIRA · <a href="https://github.com/hasanlior/polymira/blob/main/LICENSE">PolyForm Strict License</a></span>
      <span>Not affiliated with Sony, Microsoft, Nintendo, Sega or Valve.</span>
    </div>
  </footer>
  <script src="../assets/site.js"></script>
</body>
</html>
"""

os.makedirs(ROOT, exist_ok=True)
for slug in ORDER:
    io.open(os.path.join(ROOT, href(slug)), 'w', encoding='utf8', newline='\n').write(page(slug))
print('pages', len(ORDER))
