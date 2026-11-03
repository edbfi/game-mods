# Player survival and flashlight

The byte replacements below establish the selected behavior of the original
implementation. Current installation uses persistent gates: god mode hooks the
seven-byte comparison at `0x1e3714` and jumps to `0x1e372d` only when selected;
the two flashlight call gates skip consumption only when selected. Disabled gates
execute the original comparison/calls. Entry jumps remain installed during pause.

This static analysis uses the exact reconstructed PE and hashes in
[consumables analysis](consumables-analysis.md). Addresses below are RVAs unless
prefixed with `0x140`. GNU objdump's decoded runtime image establishes instruction
bytes and control flow. No game memory was changed and no gameplay damage or
battery-drain test was performed for these findings.

Ghidra CLI attempts to decompile `0x1402427e0` and `0x1401e96f0` exceeded the
330-second import/analysis startup deadline, so findings here are based on
the captured binary, decoded assembly, vtable bytes, and existing read-only
inventory observations.

## God mode

The live player vtable previously established by the read-only inventory tool is
RVA `0xa81f08`. Slot `+0xec8` points to `0x1e3710`. This player override checks
byte `+0x1344` against 2 and returns true in that state; its optional diagnostic
reads `VS regen state is ON`. Otherwise it delegates to shared predicate
`0x2427e0`, which checks global debug bit 2 and entity flags. That shared predicate
has dozens of other vtable references; it must not be patched globally.

The original implementation patched the two-byte branch at `0x1e371b` from `75 1A` to `EB 10`. This jumps
directly to the existing `mov eax,1` return at `0x1e372d`, preserving the stack
cleanup and avoiding repeated logging. Player-type vtables reference this
specific override; NPC vtables using the shared predicate remain unchanged.

This predicate participates in actual damage processing: damage method
`0x21ebc0` calls `0x224360`; that method calls the object's virtual `+0x1a8`
at `0x224425`. For the live player, `+0x1a8` is `0x2412a0`, which calls
virtual `+0xec8` at `0x2412c2` and returns false when it is true. The caller
then takes `0x22442d` directly to `0x224458`, skipping subtraction through
`0x21eec0` and health virtual `+0x10`. There is also a floor of `0.01` on
other paths at `0x224504` and `0x224538`. Thus the patch uses the existing
invulnerability decision rather than a global health-store patch.

The branch's complete unique 24-byte context begins at `0x1e3710` and is
recorded in [`src/profile.rs`](../src/profile.rs). This establishes the ordinary
damage path statically. Scripted kills, instant-death volumes, suffocation,
cutscenes, regeneration behavior, and transitions remain gameplay-test unknowns.
The patch does not restore an already dead player or promise a constant health
value across every script or mechanic.

## Flashlight and shared battery

Player equipment update `0x1e96f0` retrieves inventory slot 12 at `0x1e9716`;
its virtual `+0x4b0` exposes the battery object. Its charge is float `+0x480`
and a charging contribution is read at `+0x684`. The player's attached torch
is obtained from the reference at `+0x1760` through virtual `+0x758`.
When switched on, it computes torch consumption and calls `0x331730` at
`0x1e982e`, passing that battery and the actor. It then still calls torch
update `0x322e60` at `0x1e9839`. The latter drives torch intensity/colour;
its diagnostic says `torchlight consumes too much energy`.

The retained read-only inventory capture observes player slot 12 with interface
vtable RVA `0xb0fcd0`. That table's `+0x4b0` points to `0x1c2a70`, whose
`lea rax,[rcx-0x430]; ret` exposes the primary object without an engine call.
This corroborates the battery object's identity; it does not observe drain.

`0x331730` multiplies the supplied duration and consumption rate, subtracts
the result from battery `+0x480`, clamps to [0,1], stores it, and publishes
battery notifications. Night vision, inventory slot 5, calls the same battery
routine later at `0x1e98f2`. Both calls must be suppressed to retain charge
while either consumer is active; this also supplies unlimited night vision.

The original implementation replaced each five-byte call with NOPs. Their return values are unused; the
subsequent torch update and night-vision update remain. This does not patch
shared battery logic or unrelated NPC equipment, and it preserves manual
charging. Both contexts are unique in the reconstructed image and cover the
complete patch range. Empty batteries are not replenished. Runtime testing must
check existing low/empty charge, torch brightness, switching, manual charging,
night vision, save/load, normal drain after disabling, and NPC behavior.

## Integration

[`src/profile.rs`](../src/profile.rs) declares three sites for `god_mode` and
`unlimited_flashlight`. The two flashlight sites share one control bit. Every
context and original byte must pass the trainer's checks before it hooks them.
