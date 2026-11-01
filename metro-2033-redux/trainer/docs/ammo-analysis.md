# Ammunition analysis

These are static observations from GNU objdump and Ghidra decompilation
of the captured, reconstructed runtime image. They establish candidate code paths, not gameplay acceptance.
No offsets from another game release were used. Addresses below use preferred
image base `0x140000000`; resolve RVAs against the actual loaded module base.

## Artifact identity

| Artifact | SHA-256 |
| --- | --- |
| Original installed metro.exe | `7c35095f27beab83b31914029eca0da07991bba8630ec12ba97eec43ffc60c38` |
| Captured runtime .text, 10,614,332 bytes at RVA 0x1000 | `1f82b1598507d28b1dde205f8f41ec659b4d08b1ea5bcbdb36311941f1c73e93` |
| Reconstructed analysis PE | `c8f4d34438a5a09f304d0fbca5b868fe5397dfa37c101312b696758e5423b6c6` |

Capture timestamp: 2026-10-31T14:00:59.6087746Z. Runtime .text had zero DIR64 relocation entries to
normalize. The reconstructed PE is an analysis artifact, not a runnable release.

## Reserve transfer and eligibility

`0x1402aecd0` returns an unlimited-ammunition predicate. A nonzero weapon byte
at `this+0xbf0` or global flag `0x140f2e0f0 & 8` returns true. Otherwise the
function resolves the weapon owner through `this+0x118` and virtual slot
`0x718`, then compares the owner subobject with the player subobject. Valid
nonplayer ownership returns true; normal player ownership returns false.
Missing ownership has additional false-return paths.

`0x1402aec80` wraps that predicate. If a configuration object returned through
virtual slot `0x13e8` has byte `+0x7c & 8` set, it instead requires nonzero
reserve count from virtual slot `0x1348`. That special case must remain distinct;
its exact relationship to military-grade ammunition is unresolved.

| Candidate call RVA | Function start | Original five bytes | Static effect of true result |
| --- | --- | --- | --- |
| `0x2a5169` | `0x1402a5020` | `E8 62 9B 00 00` | Sets r14d=1. At 0x1402a5212 this bypasses reserve count getter +0xe80, decrement loop, and setter +0xe78. It adds the transferred count to the magazine and still reaches magazine setter +0x1350 or +0x10e0. |
| `0x2aa6f8` | `0x1402aa5f0` | `E8 D3 45 00 00` | Sets r12d=1. At 0x1402aa89b this bypasses inventory removal through 0x14029cc30 and owner notification +0x1278; later magazine/state processing continues. |
| `0x2a2ced` | `0x1402a2ca0` | `E8 DE BF 00 00` | Allows action eligibility despite empty reserve; false otherwise clears action state via 0x140255fe0 and rejects the action. No count decrement occurs here. |

Each following instruction-aligned signature occurs exactly once in the
reconstructed PE. The call starts at signature offset zero:

```text
E8 62 9B 00 00 84 C0 74 08 41 BE 01 00 00 00 EB 61
E8 D3 45 00 00 45 33 E4 84 C0 41 0F 95 C4 48 85 DB
E8 DE BF 00 00 84 C0 75 11 48 8D 8B 10 07 00 00 33 D2
```

A more complete candidate is replacing the first instruction at checker RVA
`0x2aecd0`, original `48 89 5C 24 08`, with `B0 01 C3 90 90`
(`MOV AL,1; RET; NOP; NOP`). It returns before entering the prologue, preserving
stack and nonvolatile registers. Unique entry signature:
`48 89 5C 24 08 57 48 83 EC 20 80 B9 F0 0B 00 00 00 48 8B D9`.
No absolute QWORD pointer references to this checker were found; the disassembly
contains five direct calls, including the wrapper. This entry candidate covers
the wrapper eligibility paths as well as direct consumers, unlike the three
individual call candidates. It also changes missing-owner returns to true.

For the individual-call alternative, the five-byte replacement
`B0 01 90 90 90` supplies AL=true. Each site consumes
only AL before assigning the predicate to a local boolean or branching. It
preserves other registers and leaves flags unchanged until the existing TEST
instruction. The original checker performs virtual queries; absence of side
effects in every possible implementation has not been established.

### Hellsing reload

The Hellsing's reload-transfer slot `+0x12f0` (`0x1402b9940`) never calls the
predicate, so the entry patch does not reach it. It looks up the arrow inventory item,
takes `min(capacity - loaded, item count)` into the magazine through `+0x1350`, then
subtracts that from the item and stores it through item setter `+0x1f8`.
`unlimited_ammo` replaces that `sub ebx,edi` at `0x2b9a58` (`2B DF`) with two NOPs,
so the setter stores the unchanged count. Like the forced predicate, it applies to
every owner. Unique signature, starting three bytes before the patch:
`41 FF D1 49 8B 06 2B DF 8B D3 49 8B CE FF 90 F8 01 00 00`.

Its eligibility slot `+0x12d0` (`0x1402b9d40`) first requires a nonzero arrow
count from `+0x1348` (`0x1402b91f0`) and only then tail-calls `0x1402a2ca0`. A Hellsing with
no arrows left still cannot reload; the patch keeps the arrows you have.

The six direct callers of `0x1402aec80` at `0x1401bb3eb`, `0x1402c6df3`,
`0x1402d0335`, `0x1402d6fd4`, `0x1402d7625`, and `0x1402dd42b` gate reload
availability or weapon state progression. They do not directly subtract reserve
inventory. These extra eligibility paths explain why patching only a decrement
site may fail when starting with zero reserve.

## Global flag scope

Setting global bit 8 is broader than changing the reserve-transfer call sites:

- `0x14024bda0` skips decrementing DWORD `this+0xcc` when that bit is set, then
  continues through `0x14024bdd0`. The exact resource class is unresolved.
- `0x1404e4820` returns `0x2af7` instead of summing inventory objects when the
  bit is set. Its normal path walks the pointer array at `this+0xf0`, with WORD
  count at `this+0xfa`, combining item and weapon counts.
- `0x1404e4960` skips an inventory-processing loop under the same bit.

The global bit therefore cannot establish independent ammunition, throwable,
and medkit switches. Its broader resource/counting behavior needs separate
validation before any use.

## Read-only inventory observation

The singleton chain observed in the checker is:
`manager = *(module_base + 0xfbd820)`, then `player = *(manager + 0x38)`.
The owner identity comparison uses `player+0xa0`. Null and unreadable pointers
must be handled; these values may be absent during menus or level transitions.

Ammunition code resolves an owner inventory through actor virtual slot `0x6e0`.
That is evidence of an accessor, not yet a verified direct memory offset.
Invoking game methods is not a read-only pointer walk. Independent medkit analysis identifies actor field `+0xb70` as an inventory
pointer, agreeing with the inventory receiver loaded at `0x1402aa69f`. This
supplies the candidate read-only chain `inventory = *(player + 0xb70)`.
Semantic mapping of its entries remains separate work.
The `+0xf0`/`+0xfa` inventory layout above applies to the receiver of
`0x1404e4820`; it must not be applied directly to the player pointer without
identifying that receiver.

## Remaining validation

These generic weapon methods have multiple subclasses/callers. Actual firearm,
throwable, NPC, and military-grade ammunition coverage still requires gameplay
observation. The special configuration bit may exclude some ammunition types.
Preserving reserve while firing military-grade ammunition can also affect its
currency balance, so currency isolation is not established. None of these
observations implements or validates no-reload, god mode, or unlimited currency.
Subsequent no-reload and survival work is documented separately in
[no-reload analysis](no-reload-analysis.md) and
[survival analysis](survival-analysis.md).


## Decompilation findings

Ghidra 12.1.4 corroborates the assembly findings against the reconstructed PE
hash above. These are static decompilation, not observation of game actions.

| Procedure | Supported finding |
| --- | --- |
| `0x1402a5020` | Unlimited predicate selects transfer without decrementing reserve, while magazine setters remain; special configuration bit follows a separate branch. |
| `0x1402aa5f0` | Predicate suppresses inventory removal; removal branch explicitly checks player identity and positive actor health. Owner inventory is read at actor +0xb70. |
| `0x1402aef30` | Built-in unlimited-ammunition action writes the +0xbf0 flag and maintains saved count at +0xbec. This supports the flag's intended semantic role. |
| `0x14024bda0` | Global bit 8 or a separate argument suppresses the +0xcc decrement before normal continuation. |

Decompiler types and names are recovered approximations; original PDB loading
is unknown. The `0x1402aef30` record additionally reports a Ghidra terminal-flow
limitation around its diagnostic call, so its complete fallthrough model is not
assumed authoritative. Assembly remains the source for exact instruction bytes,
entry-patch length, and stack/register reasoning. No proprietary full
pseudocode is reproduced here.
