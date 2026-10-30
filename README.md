# HORIZON OFFLINE

[Русская версия](README.ru.md)

<img src="assets/horizon-offline.png" alt="Horizon Offline" width="96">

**Minecraft on Rust, playable with a cracked installation too.**

Horizon Offline by **RRM Network** runs the native [Horizon](https://github.com/horizon-rust/horizon)
client using files from your existing Minecraft installation, including third-party
launchers such as TLauncher. No Microsoft/Mojang sign-in is required by this launcher.

**[Download for Windows x64](https://github.com/holodse-web/horizon-offline/releases/latest)** · v0.8.0 · Minecraft Java 26.3

> [!WARNING]
> **Play in English.** Russian currently works poorly and can cause very long loading times.

## How to play

1. Download **Horizon-Offline-0.8.0.zip** from Releases and extract it.
2. Keep **horizon-offline.exe** and **horizon-launcher.exe** in the same writable folder.
3. Run **horizon-offline.exe**, set your nickname with **2**, then press **1 / Enter**.

You need an existing **Minecraft Java 26.3** installation and **JDK 25+** for the
first resource setup. If Minecraft is not found, select its folder with **4**.
Set `JAVA_HOME` if your JDK is not detected. First setup needs internet and may take
several minutes; after that, the native game can run offline. Minecraft game files
are not included. Java/Fabric mod JARs are not supported.

## What it does

- Checks GitHub at startup and installs newer versions of **Horizon Offline**, the
  **official Horizon launcher**, and your already-installed **Horizon game client**.
  Updates to Horizon Offline restart the program automatically. Without internet,
  the installed game remains available. **8** retries program/launcher updates;
  **6** installs or updates the game client.
- Downloads full release archives, but replaces only changed client files on disk.
  Compatible prepared resources are reused. If everything is current, it says so.
- Imports Minecraft settings on first play if Horizon has none. Use **7** to import
  again; previous settings are backed up. **Language is copied too — choose English.**
- Shows download progress and resource preparation activity, with a short logo animation.

**Other keys:** **2** nickname · **3** check files · **4** Minecraft folder ·
**5** game folder · **0 / Q** exit. The menu is in Russian.

## Your files

Data is stored beside the EXE in **Horizon Offline Data/**. Existing **Horizon Rust Data/**
folders are detected automatically. Worlds and settings live in `game/`; nickname
and Minecraft path are in `horizon-rust.json`. Updates preserve these files.
Program backups are in `.horizon-updates/`, client backups in the data folder's
`.rust-updates/`. Close other launcher windows before updating.

Run with `--no-update` to skip startup updates for one run, or `--updates-check` to
check program versions without installing. `HORIZON_OFFLINE_NO_ANIMATION=1` disables
motion; the Windows animation preference is also respected.

## Build and credits

Requires **Rust 1.86+**, a Windows x64 linker, and Windows SDK `rc.exe` (MSVC) or
MinGW-w64 `windres.exe` (GNU). In **PowerShell**, run `./Build.ps1`.
The script tests and builds the wrapper without starting the game. Keep the official
`horizon-launcher.exe` from the release beside your build. Source archives are not
ready-to-play packages.

Based on Horizon under [MIT](LICENSE). [Third-party notices](THIRD_PARTY_NOTICES.md)
and [embedded license texts](assets/THIRD_PARTY_LICENSES.txt) are also available via
`horizon-offline.exe --licenses`. Not affiliated with Mojang or Microsoft.
