# HORIZON OFFLINE

[Русская версия](README.ru.md)

<img src="assets/horizon-offline.png" alt="Horizon Offline" width="96">

**by RRM Network** · v0.7.2 · Windows x64 · Minecraft Java 26.3

An unofficial terminal launcher for the native Rust client
[Horizon](https://github.com/horizon-rust/horizon). Play with an offline nickname,
import your existing Minecraft settings, and install updates without replacing
unchanged files. The launcher menu is in Russian.

**[Download the latest release](https://github.com/holodse-web/horizon-offline/releases/latest)**

> [!WARNING]
> **Play in English.** The Russian in-game language currently works very poorly
> and can cause very long loading times. English is strongly recommended.
> This concerns the game language; the launcher menu can remain in Russian.

## Quick start

1. Download **`Horizon-Offline-0.7.2.zip`** from Releases and extract it to a writable
   folder. Keep `horizon-offline.exe` and `horizon-launcher.exe` together.
2. You need your own installed copy of **Minecraft Java 26.3**. Run it once with
   your regular launcher so its files are available.
3. Open **`horizon-offline.exe`**. If automatic detection fails, use **4** to select
   your `.minecraft` or TLauncher game folder.
4. Set your nickname with **2**, then press **1** or **Enter** to play.

First setup downloads Horizon and prepares resources from your existing Minecraft
files. It can take several minutes and requires **JDK 25+** for the Java exporters.
Set `JAVA_HOME` to your JDK if it is not detected. Once setup is complete, the
native Rust game can run offline. Installation and update checks need internet.
Java/Fabric mod JARs are not supported by this native client.

## Menu

| Key | Action |
|---|---|
| **1 / Enter** | Play |
| **2** | Change nickname: 3–16 ASCII letters, digits or `_` |
| **3** | Check client files and resources |
| **4** | Select an existing Minecraft folder; leave empty for automatic detection |
| **5** | Open the game folder |
| **6** | Install Horizon or check for updates |
| **7** | Import Minecraft settings |
| **0 / Q** | Exit |

Settings are imported from your selected Minecraft's `options.txt`, including
language, controls, audio and graphics options supported by Horizon. Import is
automatic on first play if Horizon has no settings yet. Use **7** to import again;
the previous file is backed up in `game/.options-backups/`. Your original
Minecraft installation is left untouched. **Language is copied too:** select
English before importing, or change it afterwards in the game.

## Updates and local data

If you already have the latest version, the launcher shows **«Уже обновлено»**
(already up to date) and skips installation. Release downloads and signature
checks use the original `horizon-launcher.exe`.

Updates replace changed files and add missing ones. Identical files stay in
place; only obsolete files managed by the previous installation are removed.
Other user files are preserved. **The full upstream release ZIP is still
downloaded**; installation on disk is incremental. Compatible prepared resources
are reused. If their format changes, the old resources are backed up before
preparing replacements. Failed update application rolls back to the previous version.

Portable data lives beside the EXE in **`Horizon Offline Data/`**:

- `game/` — worlds, settings, screenshots and game logs;
- `versions/` — installed client and prepared resources;
- `.rust-updates/` — update journal and backups of changed files.

Nickname and Minecraft path: `horizon-rust.json`. Startup errors:
`horizon-offline.log`. Existing **`Horizon Rust Data/`** folders are detected
automatically. Personal data and build outputs are excluded from Git.

## Build from source

Requires **Rust 1.86+**, a Windows x64 toolchain and its linker. In **PowerShell**:

```powershell
.\Build.ps1
```

Icon embedding needs Windows SDK `rc.exe` for MSVC or MinGW-w64 `windres.exe`
for GNU. Set `RC` to override the resource compiler path. The script runs unit
tests, builds the EXE and checks the menu in isolated folders without starting
the game. Source is in `src/`; the full Horizon source checkout is not required.

The repository contains source code. To run your build, place the original
**`horizon-launcher.exe`** from the release ZIP beside `horizon-offline.exe`.
For a ready-to-use installation, download our release ZIP rather than GitHub's
automatically generated **Source code** archives.

## Credits and license

Based on [Horizon](https://github.com/horizon-rust/horizon), under the MIT license.
See [LICENSE](LICENSE), [third-party notices](THIRD_PARTY_NOTICES.md) and
[dependency licenses](licenses/). The release includes the unmodified official
Horizon launcher. Minecraft JARs, game assets, accounts and player saves are
not included. Minecraft is a trademark of Mojang and Microsoft; this project is
not affiliated with or endorsed by them.
