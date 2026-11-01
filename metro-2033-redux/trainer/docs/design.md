# Trainer design

The mod is a Windows x64 console companion. It reads an INI once, waits for the
Steam build of Metro 2033 Redux in any install folder, and changes only the
running process. It does not replace a game DLL, modify `metro.exe`, unpack the
installed files, or edit saves.
The console reports status. Configurable hotkeys toggle features during play;
there is no overlay. Quiet generated WAV chimes indicate successful changes.

The UI is the sole console input-record consumer. Game-focused hotkeys use
global asynchronous rising edges. Outside the game, each edge requires a
matching console key-down record and exact modifiers during the same physical
hold. Either can arrive first; a match toggles once. Releases, modifier changes,
focus records and incomplete input batches cancel pending holds. Presentation
updates do not consume input. No terminal process or host window is whitelisted.
Focus records only cancel: Microsoft documents [FOCUS_EVENT_RECORD](https://learn.microsoft.com/en-us/windows/console/focus-event-record-str)
as internal with a reserved field. [Console key records](https://learn.microsoft.com/en-us/windows/console/key-event-record-str)
carry the key and exact modifier state. [GetConsoleWindow](https://learn.microsoft.com/en-us/windows/console/getconsolewindow)
returns an undisplayed message window under ConPTY, so its system-menu style
cannot ensure host Alt+Space passthrough.

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

The INI defines initial selections and keybinds. The master defaults active after
successful installation; an explicit `enabled=false` starts paused. Feature
defaults remain unchanged. Controls stay zero throughout preparation/installation.
Master pause remembers selections and publishes zero. All fourteen entries stay
installed. Every toggle uses one local InterlockedExchange on a pagefile-backed
section mapped RW in both processes. An unchanged effective mask performs no
publication. No toggle takes thread snapshots, suspends threads, writes executable
memory, changes protections, or flushes instruction caches.

Each ordinary hook snapshots the aligned DWORD once and restores gate scratch
registers and flags before its path. Bits 0–6 select ordinary features; bits 7–30
select the 24 weapon classes. Global no-reload expands to all class bits. The
existing four no-reload hooks retain their class/owner checks and disabled fast
path. No injected worker or heartbeat is needed. Prepared code pages are RX.

Ordinary direct calls transfer with the original continuation pushed on the stack,
then jump to the original callee (or throwable action), preserving original shadow
space and alignment. Mask damage uses the same scheme for its virtual call. No
ordinary call returns into a cave. The filter gate touches no SIMD state; its paths
perform the original MULSS or the existing enabled XORPS respectively. God mode
hooks the seven-byte comparison before the short branch, so disabled mode rejoins
the original branch with comparison flags. Hellsing reserve hooks the preceding
three-byte load plus the two-byte subtraction; enabled mode retains the load.
These preserve existing feature semantics rather than adding Hellsing fixes.

Installation and graceful detach remain guarded transactions. Detach publishes zero
before restoring owned entry bytes and preserves foreign bytes on failure. Caves
and remote controls survive detach because existing no-reload setter calls can
return into caves. Windows frees them at game exit; local views and handles close
on trainer shutdown. Each attachment retains 56 KiB of code plus the shared control
view (mapping granularity may reserve more). Forced termination needs a game restart.

No gameplay frame-time measurement establishes that stutter is resolved.
Coherent publication requires Windows 10 version 1703 or newer.

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

Shared publication follows Microsoft's guarantees for coherent views in
[MapViewOfFile](https://learn.microsoft.com/en-us/windows/win32/api/memoryapi/nf-memoryapi-mapviewoffile)
and cross-process atomic aligned 32-bit access in
[Interlocked Variable Access](https://learn.microsoft.com/en-us/windows/win32/sync/interlocked-variable-access).
[MapViewOfFile2](https://learn.microsoft.com/en-us/windows/win32/api/memoryapi/nf-memoryapi-mapviewoffile2)
is inline, so the implementation dynamically resolves its documented backing
MapViewOfFileNuma2 API. Unsupported systems fail before entry installation;
there is no weaker publication fallback. The declared pointer signature avoids
requiring newer MinGW header declarations under the pinned Zig toolchain.
