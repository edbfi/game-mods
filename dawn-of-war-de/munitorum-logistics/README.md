# Munitorum Logistics

An always-on wincondition for Warhammer 40,000: Dawn of War – Definitive Edition. While you hold at least 1000
requisition, it spends it on your squads: it reinforces them to full strength, recruits missing leaders, and buys a
varied mix of weapon upgrades that your tech allows. It acts for the local player only.

## Install

[Download](https://github.com/edbfi/game-mods#download) a release or nightly zip. Install it with Vortex, or unzip it
into the game folder (`…\steamapps\common\Dawn of War Definitive Edition`) so its `W40k`, `WXP`, `DXP2` and `DXP3`
folders merge with the game's. It then shows up as the **Munitorum Logistics** game rule.

Each module folder carries the same script with that module's unit catalog. Same-path files shadow each other
rather than stack, so a game loads one copy: the one from the newest module in its stack.

## Build

- `just build` writes `build/<Module>/Data/scar/winconditions/autoreinforce.scar` from
  `src/autoreinforce.template.scar` and `src/catalogs/<Module>.scar`.
- `just test` syntax-checks the output with `luac5.1`.
- `just package 0.1.0` writes `dist/dawn-of-war-de-munitorum-logistics-0.1.0.zip`.

Dawn of War runs Lua 5.0: use `table.getn`, not `#`, and `math.mod` or `math.floor`, not `%`.

The catalogs were generated once from the game's data by a local tool and are maintained by hand.

Started from Easy Reinforcement 0.1.2 by shadowevor (Nexus #49); rewritten.
