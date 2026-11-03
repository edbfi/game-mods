# Metro 2033 Redux trainer

A Windows x64 trainer (Windows 10 version 1703 or newer) for the Steam build of Metro 2033 Redux, from any install
folder. It waits for the game, checks that it is the one supported build, and hooks a few of its functions in memory.
Every cheat can be switched on and off while you play, from the game or from the trainer's dashboard. Nothing on disk
changes: no game files, no saves and no overlay.

**Status:** experimental. The trainer refuses any other build of the game and any code another mod has changed.
[docs/validation.md](docs/validation.md) records which features have passed a gameplay check. Don't assume one
has passed unless that page says so.

| Cheat | What it does |
| --- | --- |
| Unlimited ammo | Keeps reserve ammo. You still reload. |
| Unlimited throwables | Keeps throwing knives, hand grenades and incendiaries. |
| Unlimited medkits | Keeps medkits when you heal. |
| Unlimited filters | Stops gas-mask filter time running down. |
| Indestructible mask | Stops damage to the gas mask. |
| No reload, all weapons | Keeps the loaded rounds of every weapon. Each weapon also has its own switch. |
| God mode | Uses the player's own invulnerability check. |
| Unlimited flashlight | Stops the battery draining, for the flashlight and night vision. |

The cheats keep what you have: they don't give you missing items, mend a broken mask, refill an empty filter or
charge a flat battery. **Keep Hellsing pressure** (on by default) holds the Hellsing's normal pressure while unlimited
ammo or a no-reload cheat covers it; turn it off to keep pumping by hand.

## Install

1. [Download](https://github.com/edbfi/game-mods#download) a release or nightly zip and unzip it anywhere. It holds
   `metro-trainer.exe`, this README and the license. Back up your saves before trying an experimental mod.
2. Run `metro-trainer.exe` in Windows Terminal or a console window, then start the game through Steam as usual.
   Starting the trainer after the game works too.
3. Once the dashboard says **ATTACHED · ACTIVE**, the cheats you picked are on.
4. To quit, press **Q** (or Esc); while the trainer is connected, choose **Quit** and press Enter. The trainer puts
   the game's code back as it was before it exits. Closing its window,
   signing out and shutting down restore it too, within the few seconds Windows allows. When the game closes, the
   trainer waits for it to start again.

The dashboard fits windows down to 40×12 characters; smaller ones get a status line. **Up/Down** moves, **Tab**
switches section, **Space** toggles the row, **R** sets the row's hotkey, **P** pauses every cheat and **?** shows the
rest. Ctrl+C opens the same quit dialog, and a second Ctrl+C quits.

## Hotkeys

| Hotkey | Cheat |
| --- | --- |
| Alt+F8 | Master switch: pause or resume every cheat, keeping your choices |
| Alt+F1 | Unlimited ammo |
| Alt+F2 | Unlimited throwables |
| Alt+F3 | Unlimited medkits |
| Alt+F6 | Unlimited filters |
| Alt+F5 | Indestructible mask |
| Alt+1 | No reload, all weapons |
| Alt+2 | God mode |
| Alt+3 | Unlimited flashlight |

The per-weapon no-reload switches start unbound. Press **R** on any row, then the new chord: a key with any of Ctrl,
Alt and Shift, such as `Ctrl+F1`, `Alt+Num5` or `Shift+F2`. The trainer refuses chords Windows uses itself (Alt+F4, Alt+Space), Ctrl+C, and chords that
would fire while you type.

Hotkeys act while the game or the trainer is in front. The game still sees every key, and an overlay such as
NVIDIA's or Steam's may use the same one: rebind any that clash. Each change plays a short chime, higher for on and
lower for off; turn **Chimes** off in the dashboard to mute them.

## Settings and log

The trainer writes its settings on first run to `%APPDATA%\edbfi\metro-2033-redux\trainer\settings.toml` and saves
every change you make in the dashboard. You can edit the file while the trainer is closed. The trainer checks every
key, and it never discards a file it can't read: it shows what's wrong and offers to reload or reset it. While the
trainer runs, it keeps an edit made by another program and tells you. **Start paused** starts the master switch
paused; pause itself is never saved.

The log is `%LOCALAPPDATA%\edbfi\metro-2033-redux\trainer\trainer.log`. `metro-trainer --help` prints both paths.

## Command line

```text
metro-trainer [--pid N] [--plain] [--verify-restore] [--colour MODE] [--glyphs MODE]
metro-trainer check [--pid N]
metro-trainer stop [--pid N]
```

- `check` tests the running game without changing it: its build, its code and every hook site.
- `stop` asks the trainer connected to each running game to restore the game's code and exit.
- `--pid N` picks one game when several run. `--plain` prints lines instead of the dashboard. `--verify-restore` also
  checks the game's whole code section after restoring it. `--colour` (`auto`, `16`, `mono`, `truecolor`) and
  `--glyphs` (`auto`, `ascii`) override the terminal detection.

`metro-trainer --help` lists the exit codes. If the trainer is killed while the game runs, or reports that hooks may
remain, restart the game before connecting again.

## Build and test

Run these from this folder with [just](https://github.com/casey/just). They need the Rust toolchain that
`rust-toolchain.toml` pins; off Windows, the build also needs [cargo-xwin](https://github.com/rust-cross/cargo-xwin)
0.23.1 and LLVM's `clang-cl`, `llvm-rc` and `lld-link` on `PATH`, with the `XWIN_*` variables that
[CI](../../.github/workflows/ci.yml) sets.

- `just test` runs the trainer's and the shared crates' tests. On x86-64 Linux they also run every generated hook
  against a stand-in game; on Windows they also install and restore hooks in a dummy game process.
- `just build` builds `build/metro-trainer.exe`.
- `just package 0.1.0` builds version 0.1.0 and writes `dist/metro-2033-redux-trainer-0.1.0.zip`.
- `just demo` shows the dashboard against a simulated game, on any OS.

With a capture of the game's unpacked `.text`,
`EDBFI_TEXT_CAPTURE=<file> cargo test --locked -p metro-2033-redux-trainer --test profile -- --ignored` checks that every hook
site matches it exactly once. The capture is game code: keep it out of the repo.

[src/profile.rs](src/profile.rs) holds the supported build's hashes and every hook site. The analyses behind them:
[ammo](docs/ammo-analysis.md), [consumables](docs/consumables-analysis.md), [gas mask](docs/gasmask-analysis.md),
[no reload](docs/no-reload-analysis.md) and [survival](docs/survival-analysis.md). A game update needs new
analysis, not just a new hash.

Game binaries, captures and extracted assets never go in the repo or a release.
