# Metro 2033 Redux INI trainer

Windows x64 trainer for the Steam build of Metro 2033 Redux, from any install folder.

Configure features before starting the companion executable. It waits for the
game and applies reversible memory patches; no overlay, game-file replacement,
or save-file editing is involved.

**Status:** build-specific experimental implementation. Static analysis and
read-only checks establish the patch locations. Gameplay validation is recorded
separately in [docs/validation.md](docs/validation.md); do not assume a feature has
passed a gameplay check unless that document says so.

| INI option | Intended effect |
| --- | --- |
| `unlimited_ammo` | Preserve reserve ammunition; reloading remains necessary |
| `unlimited_throwables` | Preserve throwing knives, hand grenades and incendiaries |
| `unlimited_medkits` | Preserve medkits when healing |
| `unlimited_filters` | Stop filter-time consumption |
| `indestructible_mask` | Prevent gas-mask damage |
| `no_reload` | Preserve loaded ammunition across supported weapons |
| `unlimited_flashlight` | Stop flashlight battery drain |
| `god_mode` | Enable the player's invulnerability check |

Unlimited currency remains reserved; setting it to `true` produces an error.
The trainer preserves items you
already have; it does not grant missing equipment, repair an already broken
mask, or refill an exhausted filter.

## Use

1. [Download](https://github.com/edbfi/game-mods#download) a release or nightly zip and unzip it. Keep
   `metro-trainer.exe` and `metro-trainer.ini` together in their own folder.
   Keep your original saves backed up before testing an experimental mod.
2. Edit the INI: set `[trainer] enabled=true` and enable the desired options under
   `[features]`. The supplied example starts with everything disabled.
3. Run `metro-trainer.exe`, then launch the game normally through Steam. Starting
   the trainer after the game is also supported. Keep the console open.
4. Wait for the console to report that hotkeys are ready.
5. Press **Ctrl+C** in the trainer console to restore the original instructions
   before exiting the trainer. Closing the game also discards all memory patches.

Feature values are `true` or `false`. Keys and hotkey names are case-sensitive.
INI edits take effect on the next trainer launch; hotkeys toggle the current
session immediately. The log is written beside the executable.
Do not force-close the trainer while the game is running; if that happens,
restart the game before another trainer run. It refuses to attach to unknown
builds or code modified by conflicting mods.

`metro-trainer.exe --check` performs read-only identity and signature validation
against an already running game, and fails if the game is not running. It does
not activate features.

`metro-trainer.exe --stop` asks the active trainer to restore its patches and
exit. Check the log for `Original bytes restored` before starting another instance.

## Keybinds and sound

Edit the `[hotkeys]` section to change bindings. Defaults are:

| Key | Toggle |
| --- | --- |
| Alt+Space | Master enable/pause (remembers individual selections) |
| Alt+1 | Global no reload |
| Alt+2 | God mode |
| Alt+3 | Unlimited flashlight |
| Alt+F1 | Unlimited reserve ammo |
| Alt+F2 | Unlimited throwables |
| Alt+F3 | Unlimited medkits |
| Alt+F4 | Unlimited filters |
| Alt+F5 | Indestructible mask |

Bindings accept `F1`–`F24`, `A`–`Z`, `0`–`9`, `SPACE`, `INSERT`, `DELETE`,
`HOME`, `END`, `PAGEUP`, `PAGEDOWN`, `PAUSE`, or `NONE`. Add `CTRL`, `ALT`,
and/or `SHIFT` with `+` or `-`, such as `CTRL+1` or `CTRL-SHIFT-F1`.
Modifiers must match exactly. Bindings must be unique;
`NONE` leaves an option unbound. Keys act once per press while the game or
trainer console is focused. Choose keys that do not conflict with your game
controls. Hotkey changes are session-only and do not rewrite the INI.

Successful changes play a quiet, short sine chime: higher for enabling, lower
for disabling. Set `[trainer] sounds=false` to mute them. A paused trainer
still accepts hotkeys; feature selections take effect when you enable it.

Individual no-reload options appear under both `[features]` and `[hotkeys]` in
the example INI. They start disabled and unbound. For example:

```ini
[features]
no_reload=false
no_reload_revolver=true
no_reload_ak74=false

[hotkeys]
no_reload_revolver=ALT+4
no_reload_ak74=ALT+5
```

Use those entries within your existing sections; do not add duplicate sections.
Global `no_reload=true` overrides individual selections. Turning global no
reload off returns to the individual selections you made. The option names use
the game's internal weapon classes (`ak74`, `revolver`, `uboynicheg`, `duplet`,
`tihar`, `helsing`, etc.). Some classes exist in the engine but may not be
available in your campaign. The setting does not grant a weapon or load an
empty magazine; load ammunition once before enabling it. Air-pressure and
weapon-charge mechanics are separate from loaded ammunition.

Unlimited flashlight also prevents night-vision drain because both use the same
battery. Recharge an already empty or dim battery once. God mode uses the
player's normal damage protection; scripted deaths and other exceptional damage
paths still require gameplay validation.

## Build and test

Run these from this folder with [just](https://github.com/casey/just); `just` lists them.

- `just test` builds and runs the portable core and patch-profile tests, then
  executes the generated no-reload hooks against synthetic objects (x86-64 Linux/WSL).
  It needs a C++17 compiler; override it with `CXX`, `CPPFLAGS` and `CXXFLAGS`.
- `just build` cross-builds `build/metro-trainer.exe` with Zig **0.17.0** from
  [the official distribution](https://ziglang.org/download/) (verify the archive
  checksum). Put it on `PATH` or set `ZIG_BIN`. No game files are needed.
- `just package 0.1.0` writes `dist/metro-2033-redux-trainer-0.1.0.zip` with the
  executable, the example INI, this README and the license.
- `just test-windows` checks the Windows memory backend against a disposable
  process. It needs WSL with Windows interop.

See [design and lifecycle](docs/design.md), [ammunition analysis](docs/ammo-analysis.md),
[consumable analysis](docs/consumables-analysis.md),
[gas-mask analysis](docs/gasmask-analysis.md), [no-reload analysis](docs/no-reload-analysis.md),
[survival analysis](docs/survival-analysis.md) and [validation](docs/validation.md).
Supported disk and unpacked-code hashes and instruction profiles are in
`include/metro/build_profile.hpp`. A new game update requires renewed analysis;
changing only the hash is insufficient.

Game binaries, captures and extracted assets never go in the repo or a release.
