# Trainer validation

A green build or a matching signature does not show that a cheat behaves correctly in the game. This page records
what the trainer has been checked against. Every cheat starts off on first run.

The C++ trainer that this one replaced hooked the same sites. It passed a live install and restore on 2026-10-31 and
an initial gameplay check, and its unlimited ammo worked in shooting-range challenges, which is why the dashboard marks
that cheat `gameplay partial`. That was different code, so none of it passes a row below.

## Owner checks

Only the owner's machine has the game, the overlays and the hardware, so these stay PENDING until the owner records
them. Record the date, the SHA-256 of the `metro-trainer.exe` used and the commit it was built from, and what was
seen. The trainer ships, as a nightly or a release, only after the check, gameplay pass, hotkeys, overlay and
terminal rows pass.

| Check | Status | Scope |
| --- | --- | --- |
| `metro-trainer check` against the supported game | PENDING | Exits 0 and reports the build, the code and every hook site as expected, with the game unchanged. |
| Gameplay pass | PENDING | Connect, toggle each cheat, quit; the game's code restored. Then the [runtime acceptance](#runtime-acceptance) rows. |
| Hook sites in a `.text` capture | PENDING | `EDBFI_TEXT_CAPTURE=<file> cargo test --locked -p metro-2033-redux-trainer --test profile -- --ignored` passes. |
| Attach from any install folder | PENDING | The game installed outside the default Steam library connects. |
| Unlimited ammo | PARTIAL | Worked in shooting-range challenges with the C++ trainer. The Rust trainer and currency isolation are unchecked. |
| Unlimited throwables, medkits, filters; indestructible mask | PENDING | Per-feature acceptance below. |
| Global and individual no reload | PENDING | Per-weapon and transition checks. |
| Hellsing pressure | PENDING | See [pressure acceptance](#pressure-acceptance). |
| God mode | PENDING | Scripted kills, suffocation, falls and transitions. |
| Unlimited flashlight | PENDING | Flashlight and night-vision drain. |
| Feature and currency isolation | PENDING | No gameplay comparison yet. |
| Stutter and freeze time | UNRESOLVED | The log records how long connecting took and how long restoring held the game's threads. No gameplay measurement yet. |
| Hotkeys in the game | PENDING | Raw Input with the game fullscreen; AltGr and AZERTY; NumLock and the numpad; unplugging a keyboard; an elevated game with an unelevated trainer. |
| Overlay clashes | PENDING | Every default hotkey against Metro's own controls and the installed Steam and GPU overlays. |
| Terminals | PENDING | The dashboard in conhost and Windows Terminal: glyphs, colours, resizing, 40×12; closing a conhost window restores the game. |

## Before connecting

- Keep a separate save for testing. Note the loaded rounds, reserve ammo, throwables, medkits, health and
  military-grade ammo.
- Check the unmodified game at the same checkpoint before testing a feature.
- A new game build needs a new profile with fresh analysis. Never reuse an offset only because another build had it.

## Runtime acceptance

Test each feature on its own before testing combinations.

| Feature | Required behaviour |
| --- | --- |
| Unlimited ammo | Firing reduces loaded rounds. An empty weapon still needs its normal reload. Reloading fills the magazine without reducing reserve ammo. Check several weapons, ammo types and part-filled magazines. |
| Unlimited throwables | Throwing still plays its animation and creates the throwable, while the count stays the same. Check each type. |
| Unlimited filters | Filter time stays constant while wearing the mask in a hazardous area. Check active filter time and the reserve, changing filters, and behaviour after switching the cheat off. |
| Indestructible mask | An intact mask keeps its durability under supported damage; ordinary player damage, wearing, removing and filter changes still work. A broken mask is not repaired. |
| Unlimited medkits | Using a medkit still takes the normal time and heals normally, while the count stays the same. Check while injured and at full health. |
| Feature isolation | Switched-off cheats behave as the game does. Enemy ammo, enemy health, pickups, shops and unrelated inventory are unchanged. |
| Currency isolation | Military-grade ammo, which is also currency, is tracked apart from ordinary reserve ammo. Watch spending, buying and any weapon mode that fires it: unlimited ammo must not make currency unlimited. |
| Restoration | Switching a cheat off restores the game's behaviour; quitting restores every hooked site. Playing on afterwards behaves as the unmodified game. If another program changes a hooked site, restoring reports that site, leaves its bytes alone and asks for a game restart. |
| Global and individual no reload | Loaded ammo stays constant only for the selected player weapons. Switch global no reload off to test single weapons; switching weapons must pick the right one. Unselected weapons and NPCs consume as usual. Check empty magazines, alternate ammo, shotguns, pneumatic weapons and reload animations. |
| God mode | Ordinary damage does not reduce the player's health; enemies stay vulnerable. Check scripted kills, suffocation, falls and transitions. |
| Unlimited flashlight | Battery charge stays constant with the flashlight or night vision on. Manual charging and the light controls still work; normal drain returns when the cheat is off. An empty battery needs charging once. |
| Hotkeys and chimes | Each hotkey toggles once per press while the game or the trainer is in front. Held keys and typing in another program change nothing. Pausing with the master switch turns every cheat off and keeps the choices; resuming brings them back. Each change chimes, higher for on and lower for off; switching Chimes off mutes them. |
| Terminal routing | Bind a cheat to Ctrl+1. It toggles once per press in the trainer's Windows Terminal tab and in the game, and not from another tab of the same window, another terminal window or an unrelated program. Repeat while holding the chord across focus changes, and in conhost. A tap released very quickly in a Windows Terminal tab may be missed. |
| Lifecycle | After the game restarts, the trainer connects to the new process and checks its build again. Loading a checkpoint and switching weapons don't change what a cheat covers or crash the game. Closing the trainer's window, signing out and `metro-trainer stop` each restore the game. |

## Pressure acceptance

Synthetic tests run the pressure hooks against native code; the live checks remain. With the Hellsing, test unlimited
ammo, global no reload and Hellsing no reload on their own: each must keep normal pressure. Switch each on with the
pressure already low and check that it recovers. With **Keep Hellsing pressure** off, pumping by hand must still be
needed. NPCs and other weapons keep their own pressure behaviour in every case.

## Automated checks

None of these runs the game or shows how a cheat behaves in it. CI runs all of them on every pull request.

- `just test` runs the portable tests: the profile's invariants, settings and their file faults, the input arbiter,
  the runtime against a simulated game, and the dashboard's snapshots.
- On x86-64 Linux, `just test` also runs every generated hook against a stand-in game and compares
  registers, flags, every XMM lane, MXCSR and memory with the game's own instructions, at the real RVAs.
- On Windows, the tests install, toggle and restore hooks in a dummy game process, including every write fault, a
  panic, a console close and a session end; they also identify the dummy read-only and ask its trainer to stop.
- CI's `windows` job runs the executable from the packaged zip: its version, its usage errors, `check` and `stop`
  with no game, and closing its console while it waits for the game.
