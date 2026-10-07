# CallOfWarcraft / CoDCraft

An experimental two-process passthrough mod: MW2 (2009) supplies the live gun, arms, animation and weapon simulation; Benilla draws the Warcraft world and UI. Both game processes must run at the same time.

**You must own both games and supply your own files.** This repository contains source patches, new source files, setup scripts, and user-supplied custom equipment icons. It does not contain retail game clients, game archives, extracted retail models/textures/sounds, bridge model captures, server map data, accounts or personal databases. It does not download either game.

The Warcraft data must be compatible with **1.12.1 (build 5875)**. A current retail WoW installation is not a replacement for that data. MW2 requires the original **2009 multiplayer data**. No game-data download links are provided.

## Current features

- Live MW2 gun and arms rendered as 3D geometry in Warcraft, with mouse aiming, ADS, sprint and automatic fire.
- Server-confirmed bullets use the equipped Warcraft weapon's main-hand damage path. Normal melee autoattack is suppressed for the gun controls.
- Crosshair, hit feedback, damage numbers, incoming tracers and damage direction feedback.
- Hostile humanoids wield Warcraft rifles and use the guest FPS controller with movement and LOS checks.
- Remote auto-loot for bullet kills and 64 carried inventory slots using the backpack plus four bags.
- Ray-sampled world lighting with optimized shader reads.
- Custom equipment icons, distinct military-themed gear names, and equipped-item selection of native MW2 firearms. Throwing weapons use knife artwork rather than a firearm icon. Existing Warcraft rarity and stats remain intact.
- Hidden MW2 map ambience and spatial impact audio are suppressed; native gun and host hit-feedback audio remain.
- New client-side skeletal death physics. IW4L's ragdoll API is a stub, so this feature uses the included PBD solver; its first live visual test is still pending.

## Setup on Windows

1. Install Git, PowerShell 7, Rust through rustup, and the Visual Studio C++ build tools. Upstream toolchain files select the required Rust versions.
2. Run `pwsh -File scripts/Prepare-Sources.ps1`. It fetches the three open-source projects at the revisions in `sources.json`, applies the patches and installs the new source files. It fetches source code, not game installations.
3. Run `pwsh -File scripts/Build-Clients.ps1`. This builds the modified Benilla and IW4L clients. Initial builds can take a long time.
4. Set up and build the patched local vMaNGOS server as described in [docs/server-setup.md](docs/server-setup.md). Stock or upstream prebuilt vMaNGOS binaries lack this mod's custom opcodes and cannot replace the patched server.
5. Copy `local-config.example.psd1` to `local-config.psd1`, and enter the paths to your owned games and the configured server. Your local configuration is ignored by Git. Create your own server account; no shared account is bundled.
6. Start your database and server, then double-click `Start CallOfWarcraft.cmd`. Log into Benilla normally. MW2's level may take 2–3 minutes to load; keep both clients running and play from Benilla.

Controls: mouse look; left click fires; right click aims; WASD moves; Space jumps; Left Shift sprints; Left Alt toggles the free cursor. With the cursor free, right click interacts with Warcraft objects.

## Source layout and licensing

`patches/` contains tracked changes against pinned upstream revisions. `overlays/` contains newly added source files. `sources.json` identifies every changed file and its SHA-256 checksum. The unused early standalone prototype is not part of this package.

`assets/gear/` contains only the project's supplied custom icon artwork and mod-authored equipment name/display mappings. The runtime icons are 128×128 RGBA TGA files; no original retail icons are included. `GearRoot` can point to a replacement folder with the same layout. Knife artwork does not add a native knife model or a throwing-knife attack system. Artwork provenance is separate from upstream source licensing; no ownership of Blizzard or Activision assets is claimed.

The original projects and their notices remain attributable to their authors: [Benilla](https://github.com/samwhosung/benilla) (MIT OR Apache-2.0), [IW4L](https://github.com/vladtrc/iw4L) (Apache-2.0), and [vMaNGOS](https://github.com/vmangos/core) (GPL-2.0). Component patches and overlays follow their respective upstream licenses; copies are in `licenses/`. Packaging scripts are MIT licensed. This project is not affiliated with Blizzard, Activision, or the upstream projects.

Run `pwsh -File scripts/Audit-Package.ps1` before publishing. Only add the audited package files to Git; keep game data and generated runtime files outside the published tree.
