# Consumables analysis for the supported Steam build

The direct byte replacements below describe the original implementation
and its behavioral evidence. Current installation uses persistent gates: selected
medkits bypass the inventory-mask AND; selected throwables transfer to the action
without the consumption wrapper. Disabled gates execute the original AND/call.
The entry jumps remain installed during master pause.

This records evidence for the independently controlled medkit and throwable patches. Addresses below use preferred image base `0x140000000`; profile entries use RVAs. Analysis applies only to executable SHA-256 `7c35095f27beab83b31914029eca0da07991bba8630ec12ba97eec43ffc60c38`.

## Runtime image

The executable has packed `.text` on disk and a `.bind` entry point. A read-only dump reads the normally launched exact-path process with `PROCESS_QUERY_INFORMATION | PROCESS_VM_READ`, exports the loaded section and metadata, and reconstructs an analysis copy by replacing only `.text`. It normalizes any DIR64 relocations in that section back to the preferred base; this build contains no such `.text` relocations.

Loaded `.text` spans RVA `0x1000`, length `0xa1f63c`; SHA-256 is `1f82b1598507d28b1dde205f8f41ec659b4d08b1ea5bcbdb36311941f1c73e93`. The captured process was PID 27168, actual module base `0x7ff73dac0000`. Repeated captures before and after station loading had the same hash. Loaded NT headers differed only in `OptionalHeader.ImageBase`, which contained the actual module base. The reconstructed file retains original entry-point/header information and is for analysis, not execution. Captured game bytes stay out of the repo and releases.

## Read-only inventory confirmation

A read-only inspection reads these structures without invoking engine functions:

1. Read singleton pointer at module base + `0xfbd820`.
2. Read actor pointer at singleton + `0x38`.
3. Read inventory pointer at actor + `0xb70`.
4. Read inventory item-pointer array at inventory + `0xf0`, with 16-bit count at inventory + `0xfa`.
5. Each item's slot is its byte at `+0xaa`.

The actor was `0x1a2a0be41e0`, inventory `0x1a2a1261d60`, inventory vtable RVA `0xb2d218`, array length 30. The inventory virtual `+0xc8` implementation is RVA `0x4e3b80`; it searches by slot. These pointers are observations, not persistent addresses.

| Slot | Identified item | Observed quantity | Inventory vtable RVA | Count representation |
| --- | --- | ---: | --- | --- |
| 0 | Throwing knives | 5 | `0xaee2e8` | DWORD at inventory item + `0x6fc` |
| 3 | Hand grenades | 2 | `0xadea78` | DWORD at inventory item + `0x6fc` |
| 7 | Medkits | 4 | `0xb0a860` | Popcount of five low bits at inventory item + `0x48` |
| 13 | Incendiaries | 3 | `0xadea78` | DWORD at inventory item + `0x6fc` |

The quantities matched the user's displayed inventory. The medkit mask was `0x0f`. Global debug flags at RVA `0xf2e0f0` were `0x240`; unlimited-ammo bit `0x08` was clear.

## Medkits

Medkit slot 7 is independently corroborated by actor code at RVA `0x1e9b99`: inventory virtual `+0xc8` is called with slot 7, the returned item's virtual `+0x448` retrieves the object, then RVA `0x330160` plays the `medkit_idle` event. Its event handler at RVA `0x3301a0` applies healing through the owner actor before subsequent cleanup.

The inventory interface is an object subobject at `+0x430`. Count getter RVA `0x1d75e4` subtracts `0x430` and jumps to RVA `0x330300`, which counts the five low bits at object + `0x478`. Thus inventory item + `0x48` is the same mask.

Consumption at RVA `0x3306e0` checks owner identity against the singleton actor. The player branch clears the selected index (`object + 0x484`) from the mask:

```text
RVA 330795  b8 01 00 00 00       mov eax,1
RVA 33079a  d3 e0                shl eax,cl
RVA 33079c  f7 d0                not eax
RVA 33079e  41 21 80 78 04 00 00 and DWORD PTR [r8+0x478],eax
RVA 3307a5  41 c7 80 84 04 00 00 ff 00 00 00
                                mov DWORD PTR [r8+0x484],0xff
```

The original replacement used seven NOPs for the seven-byte AND. This preserves selected-index reset and the subsequent UI cleanup tailcall. Nonplayer mask writes remain unchanged. The 27-byte context starting at RVA `0x330795` occurs once in decoded `.text`.

## Throwing knives, grenades and incendiaries

A shared component at RVA `0x24bda0` decrements its DWORD `+0xcc` unless `r9d != 0` or global unlimited-ammo bit `0x08` is set. It then tailcalls RVA `0x24bdd0`, which performs the action. Component count is serialized as a 16-bit quantity (`0xffff` means `-1`). The component is used by several unrelated classes, so globally changing it would have broader scope.

The live knife inventory vtable RVA `0xaee2e8` is installed at object `+0x430` by constructor RVA `0x1a1b28`. That constructor installs object vtable RVA `0xaed058`; its virtual `+0x2d0` is RVA `0x2e86d0`, which calls the shared decrement wrapper at RVA `0x2e870a`.

Both live grenade and incendiary inventory items share vtable RVA `0xadea78`. Constructor RVA `0x1a1c54` installs it at object `+0x430` and object vtable RVA `0xadd7c0`. That object's virtual `+0x2d0` is RVA `0x348fa0`, which tailcalls RVA `0x348b50`; it calls the decrement wrapper at RVA `0x348b92`.

Redirect these two calls from RVA `0x24bda0` to RVA `0x24bdd0`, adding `0x30` to each relative displacement:

| Call RVA | Original | Replacement |
| --- | --- | --- |
| `0x2e870a` | `e8 91 36 f6 ff` | `e8 c1 36 f6 ff` |
| `0x348b92` | `e8 09 32 f0 ff` | `e8 39 32 f0 ff` |

Unique contexts, each with patch offset 11:

```text
RVA 2e86ff: 48 8d 96 18 01 00 00 4c 89 70 e8 e8 91 36 f6 ff 48 8b 8e 18 01
RVA 348b87: c1 60 0a 00 00 41 8b f8 4c 8b e2 e8 09 32 f0 ff 49 8d b6 b0 05
```

These redirects preserve action execution and its arguments, without changing the global flag or other callers of the shared component. The inventory item's count `+0x6fc` corresponds to parent object `+0xb2c`, or embedded component `+0xa60` plus count `+0xcc`.

## Verification limits

Static control flow and read-only inventory observations support these patches. Actual use, healing, throwing, reloading, save transitions, and restoration still require controlled gameplay verification. Quantities already at zero are not replenished. No live game memory was changed during this analysis.
