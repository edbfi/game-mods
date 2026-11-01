# Trainer design

The mod is a Windows x64 console companion. It reads an INI once, waits for the
Steam build of Metro 2033 Redux in any install folder, and changes only the
running process. It does not replace a game DLL, modify `metro.exe`, unpack the
installed files, or edit saves.
The console reports status. Configurable hotkeys toggle features during play;
there is no overlay. Quiet generated WAV chimes indicate successful changes.

## Compatibility and lifecycle

The executable on disk is identified by SHA-256. Because this build unpacks code
during normal startup, the trainer also checks the loaded PE headers and a
separate fingerprint of the unpacked `.text` section. ASLR is accounted for.
Each feature uses a reviewed instruction signature and expected original bytes.
An unknown executable, mismatched instruction, or ambiguous signature prevents
activation. These checks intentionally favor refusing an incompatible game over
trying nearby offsets.

Code writes require temporarily suspending the game's threads and checking that
none is executing the affected instructions. The trainer restores page
permissions and flushes the instruction cache before resuming. Multi-site changes
are prevalidated and rolled back on failure. Graceful shutdown restores patches;
forced termination cannot guarantee cleanup, so restart the game after killing
the trainer. Closing the game discards all its memory patches.

Keep only one trainer instance active. Other mods that alter the same executable
code may be rejected by the loaded-code fingerprint. Trainer effects can affect
what the game naturally saves (for example, consumables retained during play),
even though the trainer never opens a save file.

## Feature boundaries

- Unlimited ammo means reserve ammunition is not consumed during reloads. Loaded
  rounds still decrease, and reloads remain necessary.
- Throwables and medkits have their own consumption sites and switches.
- Filter lifetime and mask damage are separate from health and ammunition.
- No reload preserves loaded ammunition separately from reserve ammunition.
- God mode uses the player-specific invulnerability predicate. Flashlight drain
  has its own switch, separate from gas filters and health.
- Military-grade ammunition/currency remains reserved and rejects `true`.

The INI defines initial selections and keybinds. Master pause restores the
original game code while remembering those selections. Hotkeys use rising-edge
detection and foreground checks; holding a key does not repeatedly toggle it.
Each change validates ownership and writes only the changed patch regions with
the game's threads suspended. Unchanged regions remain active. No-reload weapon
selectors share one contiguous write. Failure attempts to restore the
previous selection before reporting an error. Configurations and saves are not
rewritten by hotkeys.

No-reload hooks classify the weapon using its live configuration ID and require
its owner to match the player. Each firing site jumps to a generated code page;
the selectors live on a separate writable page. Three executable pages and one
control page (16 KiB total per attachment) remain allocated after detachment,
because a displaced setter call could still return through the generated code.
Original hook bytes and selectors are restored; Windows frees those pages when
the game exits. Repeated trainer restarts accumulate these small allocations
until the game restarts.

Read-only dumps are for analysis, not runnable replacements. They and the
game's proprietary assets stay out of the repo and must not be packaged
with the trainer. See [validation.md](validation.md) for the gameplay checks and
[ammo-analysis.md](ammo-analysis.md) for evidence and remaining uncertainties.

## Platform references

The companion architecture avoids initializing a mod under the game's loader
lock; Microsoft documents the restrictions in [DLL best practices](https://learn.microsoft.com/en-us/windows/win32/dlls/dynamic-link-library-best-practices).
Executable-page changes follow the protection/cache requirements documented for
[VirtualProtectEx](https://learn.microsoft.com/en-us/windows/win32/api/memoryapi/nf-memoryapi-virtualprotectex)
and [FlushInstructionCache](https://learn.microsoft.com/en-us/windows/win32/api/processthreadsapi/nf-processthreadsapi-flushinstructioncache).
