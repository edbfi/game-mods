# Gas mask and filter candidates

The byte replacements below describe the original implementation and its
behavioral evidence. Current installation uses persistent gates: the selected
filter path zeros consumption, and the selected mask path skips damage dispatch.
Disabled gates execute the original MULSS/virtual call. Entry jumps remain
installed during master pause.

Static GNU objdump analysis uses the same original executable and reconstructed
runtime hashes recorded in [ammunition analysis](ammo-analysis.md). No gameplay
behavior has been validated. RVAs resolve relative to the loaded module base.

The gas-mask constructor at preferred VA `0x140328fb0` installs primary vtable
`0x140b6c130`. Its configuration loader references `max_filter_time` and
`gasmask_health_sets`. The former populates WORD `this+0x56c`; that field is a
configuration limit, not a running countdown. Its damage configuration fields
`+0x560`, `+0x564`, and `+0x568` are `mp_hit`, `mp_hit_apply_speed`, and
`single_hit_apply_speed`.

## Filter time

Update function `0x1403290c0` loads current filter time from float
`this+0x44c`. At RVA `0x3292d1`, `MULSS xmm9,xmm2` multiplies elapsed time by
the interpolated spend rate. The subsequent `SUBSS xmm0,xmm9` decreases current
time. The same delta is passed as XMM1 to `0x140270be0`, which broadcasts to
observers at inventory array `+0x1508`, WORD count `+0x1512`. The observer
implementations and any total-pool effects remain unresolved.

Replacing the rate multiplication with `XORPS xmm9,xmm9; NOP` supplies zero to
both the timer subtraction and observer notification. Merely removing SUBSS
would leave observers receiving nonzero spent time. Neither SSE instruction
changes EFLAGS. Subsequent clamping, stores, and update processing remain.

- Patch RVA: `0x3292d1`.
- Original: `F3 44 0F 59 CA`.
- Candidate replacement: `45 0F 57 C9 90`.
- Unique instruction-aligned signature, patch at offset zero:

```text
F3 44 0F 59 CA 44 0F 28 C0 F3 44 0F 5E 86 54 04 00 00 F3 41 0F 5C C1
```

Filter replacement function `0x14032aca0` transfers filter-object float `+0x680`
into mask fields `+0x454` and `+0x44c`. Those stores are separate from normal
time spending and remain intact. Starting with an exhausted filter and changing
filters still require runtime testing.

## Mask damage

`0x14032b570` prepares a mask hit, scales its damage, then calls virtual slot
`+0x1b0` at RVA `0x32b6e9`. The installed gas-mask vtable resolves this slot to
`0x14032afc0`. That method invokes base damage `0x140246d20`, which delegates
core damage to `0x14021ebc0`, and processes mask-specific visual transitions.

Removing the six-byte virtual call bypasses that damage dispatch. Its return
value is unused; the next instructions reset XMM0 and compare existing mask
health at `this+0x308`. The wrapper continues its reference cleanup and existing
health-dependent owner callbacks. Hit visual effects dispatched by the removed
call may also be suppressed. An already broken mask is not repaired by this
candidate.

- Patch RVA: `0x32b6e9`.
- Original: `FF 90 B0 01 00 00`.
- Candidate replacement: six `90` bytes.
- Unique instruction-aligned signature, patch at offset zero:

```text
FF 90 B0 01 00 00 0F 57 C0 0F 2F 83 08 03 00 00 72 48
```

Each signature occurs exactly once in the reconstructed PE. Runtime tests must
confirm player scope, intact-mask durability under different damage sources,
filter countdown and total reserve time, normal mask removal/replacement, and
normal behavior after restoring original instructions.

## Read-only runtime inspection

The read-only inspection validated the supported on-disk hash and known actor,
inventory, and accessor vtables before walking the bounded inventory list. It
worked while trainer patches were active because it did not require original
code bytes. It never invoked engine functions or requested write rights.

The live mask is inventory slot 4, interface vtable RVA `0xafb9b0`. Subtracting
`0x430` from the item interface obtains its primary object, whose live vtable is
RVA `0xafaa90`. Constructor `0x1401a1d90` calls `0x1401a57ec`, which calls the
base mask constructor `0x140328fb0`, before installing this derived vtable.
Its damage slot `+0x1b0` points to thunk `0x1401c890c`, which jumps to
`0x14032afc0`, confirming the same mask implementation.

The station-save observation while all trainer features were active was current
filter time `180`, loaded filter duration `180`, mask health `1`, actor health
`1`. These are instantaneous readings and do not establish countdown or damage
behavior. A maximum-health field has not been established, so the script does
not report a guessed maximum. The active filter's 180 seconds is distinct from
the shop's reported total of 60:00. Neither instantaneous reading proves that
filter consumption stops in a hazardous area.
