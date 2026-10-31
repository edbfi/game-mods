# Trainer validation

A successful build or signature match does not establish correct game behaviour.
The example INI starts with every feature disabled.

## Status

| Check | Status | Scope |
| --- | --- | --- |
| Portable core tests (`just test`) | PASS | Configuration parsing, signature scanning, patch transactions, rollback and restoration failure paths. |
| Generated no-reload hooks (`just test-hooks`) | PASS | Every registered class and alias, individual selection, original decrement, NPC exclusion, null pointers, unknown classes, rel32 range rejection. Synthetic objects only. |
| Windows memory backend (`just test-windows`) | PASS | Nearby cave allocation, RX code and RW controls, repeated transitions, protection restoration, thread resume counts, foreign bytes preserved. Dummy process only. |
| Code-region signatures | PASS | All signatures match once in a captured `.text` of the supported build. |
| Live apply/restore lifecycle | PASS | 2026-10-31: all eight feature groups activated, a graceful stop restored the original regions, and a fresh whole-code hash and profile check passed. Attachment and restoration only. |
| Initial gameplay check | PASS | The owner confirmed the features and the toggle sounds behave as expected while firing and toggling. Not exhaustive. |
| Unlimited ammunition | PARTIAL | Works in shooting-range challenges (owner report). Currency isolation not established. |
| Unlimited throwables, medkits, filters; indestructible mask | PENDING | Per-feature gameplay acceptance below. |
| Global and individual no reload | PENDING | Per-weapon and transition checks. |
| God mode | PENDING | Scripted kills, suffocation, falls and transitions. |
| Unlimited flashlight | PENDING | Flashlight and night-vision drain. |
| Feature and currency isolation | PENDING | No completed gameplay comparison. |
| Stutter | NOT THE TRAINER | Stutter stayed the same with the trainer stopped and the original code restored. Cause undiagnosed. |

## Before applying patches

- Record executable SHA-256, architecture, module image size, and build/store
  identity. A profile must identify that executable and document evidence for
  each patch. Do not reuse an offset solely because another release used it.
- Confirm each signature matches exactly one intended executable-code region.
  Check expected original bytes, patch boundaries, and absence of overlapping
  patch regions. Unsupported builds or ambiguous matches must fail closed.
- Preserve a separate save for testing. Record initial loaded rounds, reserve
  ammunition, throwable counts, medkit count, health, and currency.
- Verify the unmodified baseline at the same checkpoint before testing a feature.

## Runtime acceptance

Test each feature separately before testing combinations.

| Feature | Required behavior |
| --- | --- |
| Unlimited ammunition | Firing reduces loaded rounds. An empty weapon still requires its normal reload. Reload fills the magazine without reducing the intended reserve ammunition. Check multiple weapons/ammunition types and partially filled magazines. |
| Unlimited throwables | Throwing still performs its normal animation and creates the intended throwable, while its inventory count stays unchanged. Check each supported throwable type separately. |
| Unlimited filters | Usable filter time stays constant while wearing the mask in a hazardous area. Check both active filter time and reserve total, filter changes, and behavior after disabling. |
| Indestructible mask | An intact mask retains durability under supported damage sources; ordinary player damage, wearing/removing, and filter changes remain functional. An already broken mask is not expected to be repaired. |
| Unlimited medkits | Using a medkit still consumes the normal action time and heals normally, while its inventory count stays unchanged. Check use while injured and attempted use at full health. |
| Feature isolation | Disabled features retain baseline behavior. Enemy ammunition, enemy health, pickups, shop transactions, and unrelated inventory retain baseline behavior. |
| Currency isolation | Military-grade ammunition used as currency is tracked separately from ordinary reserve ammunition. Spending, purchasing, and any weapon mode using that ammunition need explicit observation; unlimited ammunition must not silently claim unlimited currency. |
| Restoration | Disabling/stopping restores trainer-owned bytes. Repeat use afterward behaves as baseline. If another writer changes a patch region, restoration reports failure and preserves foreign bytes. |
| Global and individual no reload | Loaded ammunition remains constant only for selected player weapon types. Disable global no reload to exercise individual selections; switching weapons must select the correct class. Unselected/NPC weapons retain normal consumption. Check empty magazines, alternate ammunition, shotguns, pneumatic weapons and reload animations separately. |
| God mode | Ordinary incoming damage does not reduce player health; enemies remain vulnerable. Check scripted kills, suffocation, falls and transitions separately. |
| Unlimited flashlight | Existing battery charge stays constant with flashlight and/or night vision active. Manual charging and light controls still work; normal drain resumes when disabled. An empty battery needs charging once. |
| Hotkeys and sounds | Each configured key toggles once per press while the game/trainer has focus. Held keys and background typing must not cause unintended changes. Master pause restores code and resume remembers selections. Successful effective changes play soft enable/disable tones; sounds=false mutes them. |
| Lifecycle | Reattach after game restart resolves the new module base and validates the build again. Loading another checkpoint and switching weapons do not alter feature scope or crash the game. |

Unlimited currency remains reserved; its `true` value is rejected.

## Automated checks

None of these launch the game or establish runtime feature behaviour.

- `just test` builds and runs the portable core and profile tests, then `just test-hooks`.
- `just test-hooks` (x86-64 Linux/WSL) executes the generated no-reload code against synthetic objects.
- `./build/profile_tests <capture>` also checks unique signatures against a local capture of the game's loaded
  `.text`. The capture is game code: keep it out of the repo and releases.
- `just test-windows` (WSL with `wsl.exe` interop and Zig 0.17.0) compiles `build/windows-backend-tests.exe`,
  which patches and restores a page in its own dummy child process. No game process is opened.
