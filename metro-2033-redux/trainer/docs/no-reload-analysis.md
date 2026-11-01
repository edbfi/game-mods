# No-reload analysis

This profile uses the exact executable and captured runtime image identified in
[ammo-analysis.md](ammo-analysis.md). RVAs are relative to the loaded module.
It preserves loaded ammunition at firing decrements and leaves reserve-transfer
code separate. Static observations do not establish gameplay acceptance.

## Firing paths

The shared weapon shot routine starts at RVA `0x2a9330`. It obtains the firing
quantity through virtual slot `+0x1368`, generates shots, and subtracts that
quantity from the loaded count before invoking setter `+0x1360`. The hook at
`0x2a9a0c` spans `mov rax,[r14]; sub edi,r13d`. Selected player weapons retain
EDI while the existing setter, notifications, and remaining code continue.

Two other explicit loaded-count decrements occur in adjacent callback methods, and
one in the Hellsing's own firing method:

| Hook RVA | Original instructions | Selected behavior |
| --- | --- | --- |
| `0x2a9a0c` | `49 8B 06 41 2B FD` | Keep EDI; preserve vtable load |
| `0x2c6ef4` | `8D 50 FF FF 93 60 13 00 00` | Pass EAX unchanged to the existing setter call |
| `0x2c6fab` | `8D 50 FF 48 8B 46 F8` | Pass EAX unchanged; preserve vtable load |
| `0x2b9dce` | `48 8B CF 8D 50 FF` | Keep `mov rcx,rdi`; pass EAX unchanged |

Each full context signature in `verified_no_reload_sites()` occurs exactly once
in a capture of the loaded `.text`, at its declared RVA. The callback methods
use RCX as the weapon receiver at the hook; the shared routine uses R14 and the
Hellsing routine RDI.
Constructor evidence places the callback vtable at full object `+0x28` and
weapon vtable at `+0x20`; the callback's `rsi-8` adjustment selects that weapon
receiver.

The Hellsing (`WEAPON_HELSING`) is the only registered firearm whose fire slot
`+0x1180` (`0x2b9d90`) neither is nor calls the shared routine: it reads the loaded
count, subtracts one arrow and calls setter `+0x1360` itself, then spawns the
projectile through `0x2b9580`. Its unload slot `+0xec8` (`0x2b9ac0`) moves loaded
arrows back to inventory and is not hooked. The Tihar uses the shared routine.

No universal count-setter patch is used, because setters also run
when loading, unloading, changing ammunition, and deserializing.

## Player ownership and weapon selection

The predicate at RVA `0x2aecd0` establishes the owner handle at weapon `+0x118`:
its pointee identifies the owner actor's `+0xa0` subobject. The player comes
from `*(module+0xfbd820)` then manager `+0x38`. All hook caves require the
owner identity to equal `player+0xa0`. Null handle, owner, manager, player, or
configuration pointers take the original path. NPC weapons retain the original
decrement even when the aggregate switch is enabled.

The game's named configuration registrations at RVAs `0x2a040` through
`0x2a660` intern `WEAPON_*` names through `0x656b10` into DWORD globals.
The game itself reads the configuration pointer at weapon `+0x1b8`, then its
first DWORD, and compares it against these globals (for example at `0x264743`).
The profile compares these same live identifiers, avoiding guessed numeric enum
values. `verified_no_reload_classes()` records all named firearm registrations;
the Duplet selector includes its explicitly named 2B, 3B, and 4B registrations.
Names retained from the binary include internal variants which may not be
obtainable in normal Metro 2033 gameplay.

Read-only live captures independently corroborated both layouts on 2026-10-31:

- Weapon types: Shambler config identifier `271136605`
  matches global RVA `0xf5d0c8`; Revolver `2210759652` matches `0xf5d0d4`;
  Kalash `1904679463` matches `0xf5d090`.
- Weapon owners: all three owner handles identify the
  same observed player subobject `0x238266C70C0`.

These captures establish identity and pointer layout, not observed firing.
The separate readable name table at RVA `0xf31630` is a HUD/configuration
serialization table; its small numeric values are not used as weapon offsets
or runtime class identifiers.

## Generated code and lifecycle

`make_no_reload_image()` saves flags and RAX/RCX/RDX/R8 before classification,
loads one aligned control DWORD, and bypasses classification when disabled. It
restores them before displaced instructions, and returns through an indirect
absolute jump which consumes no scratch register. Every unselected branch
executes the displaced instructions. The selected shared branch omits SUB;
subsequent MOVs and the setter call consume no incoming arithmetic flags.
The callback LEA and replacement MOV both leave arithmetic flags unchanged.
Nonvolatile registers are untouched except for the intended EDI result in the
shared firing path. Each cave fits within one 4 KiB page.

The Windows integration must allocate caves within rel32 jump range, validate
original signatures, write and verify executable code before installing hooks,
and install entry hooks transactionally while game threads are suspended. Normal
selector changes publish a single coherent shared DWORD without suspension. Cave allocations remain
owned by the game process after hook removal, so suspended return addresses or
threads executing displaced calls cannot return into freed memory.

## Verification and limits

Reproducible native x64 verification:

```sh
c++ -std=c++20 -O2 -Wall -Wextra -Werror -I include tools/verify-no-reload.cpp -o /tmp/verify-no-reload
/tmp/verify-no-reload
```

This executes every generated cave against synthetic module/player/weapon
memory and an actual indirect setter stub. It checks selected, ordinary, and
NPC paths, every named class and alias, plus null player/configuration, unknown
class, and out-of-range cave
rejection for the shared hook. It does not write to the running game.

Gameplay validation remains necessary for weapon-specific animations, pneumatic
pressure/charge, military-grade ammunition, zero initial loaded count, and level
transitions. Preventing a firing decrement does not refill an already-empty
weapon or remove manual reload animations. Static searches establish these
four firing-decrement paths, not proof that no additional subclass-specific
resource consumption exists.
