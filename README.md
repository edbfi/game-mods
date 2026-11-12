<p align="center">
  <img src="assets/svg/game-mods-logo.svg" alt="game-mods logo" width="160" height="160">
</p>

<h1 align="center">game-mods</h1>

<p align="center"><strong>Game mods, trainers, and scripts with tested release builds.</strong></p>

<p align="center">
  <a href="https://github.com/edbfi/game-mods/blob/main/LICENSE"><img src="https://img.shields.io/badge/original%20code-AGPL--3.0--or--later-blue?style=flat" alt="Original code: AGPL-3.0-or-later (see licensing exceptions)"></a>
  <a href="https://github.com/rust-lang/rust" title="Rust source"><img src="https://img.shields.io/badge/Rust-000000?style=flat&amp;logo=rust&amp;logoColor=white" alt="Rust"></a>
  <a href="https://github.com/lua/lua" title="Lua team's official development mirror, updated irregularly"><img src="https://img.shields.io/badge/Lua-2C2D72?style=flat&amp;logo=lua&amp;logoColor=white" alt="Lua"></a>
</p>

Mods for games I play. One folder per game, one subfolder per mod.

| Game | Mod | Kind | Status | Release | Nightly | Nightly scan |
| --- | --- | --- | --- | --- | --- | --- |
| Metro 2033 Redux | [trainer](metro-2033-redux/trainer/) | Windows trainer with a terminal dashboard | Experimental | [releases](https://github.com/edbfi/game-mods/releases?q=metro-2033-redux%2Ftrainer&expanded=true) | [download](https://nightly.link/edbfi/game-mods/workflows/nightly/main/metro-2033-redux-trainer-nightly.zip) | [VirusTotal][vt-metro-2033-redux-trainer] |
| Dawn of War – Definitive Edition | [munitorum-logistics](dawn-of-war-de/munitorum-logistics/) | Always-on wincondition script | Playable | [releases](https://github.com/edbfi/game-mods/releases?q=dawn-of-war-de%2Fmunitorum-logistics&expanded=true) | [download](https://nightly.link/edbfi/game-mods/workflows/nightly/main/dawn-of-war-de-munitorum-logistics-nightly.zip) | [VirusTotal][vt-dawn-of-war-de-munitorum-logistics] |

## Download

A **release** is a version of one mod picked to keep, with a version number. A **nightly** is the latest build of
`main`: every change, built and checked by the mod's tests, but not necessarily tried in-game. Each mod's README says how to install it.

Nightly downloads go through [nightly.link](https://nightly.link). If it fails, open the
[latest successful nightly run](https://github.com/edbfi/game-mods/actions/workflows/nightly.yml?query=branch%3Amain+is%3Asuccess)
and download the mod under **Artifacts** (this needs a GitHub sign-in).

Every download is submitted to VirusTotal: a release's report links are in its notes, a nightly's in the table
above and in its run's summary. A memory-patching trainer can still trip heuristic detections.

<!-- The nightly workflow rewrites the links between these markers after each scan. -->
<!-- nightly-virustotal:start -->
[vt-dawn-of-war-de-munitorum-logistics]: https://www.virustotal.com/gui/file/a60bb65a3eadc2cada0d960a3b40bcafd47f8971d8b2bd8e3300eedaf668edb8/detection
[vt-metro-2033-redux-trainer]: https://www.virustotal.com/gui/file/db5d40b72127b2aeba82c23f8882040574680345312a70295415e41a8c4426fa/detection
<!-- nightly-virustotal:end -->

## Develop

Each mod has a `justfile` with `test`, `build` and `package <version>`. From the repo root, `just test` and
`just build` run them for every mod.

## License

Original code by edbfi, except the Munitorum Logistics sources noted below, is licensed under the
[GNU Affero General Public License](LICENSE), either version 3 or (at your option) any later version
(`AGPL-3.0-or-later`), © edbfi.

The [Munitorum Logistics sources](dawn-of-war-de/munitorum-logistics/src/) retain their existing licensing
and provenance notices, including the template's `AGPL-3.0-only` grant, game-derived catalogs and
[Easy Reinforcement credit](dawn-of-war-de/munitorum-logistics/README.md).
The bundled [REA skill](.agents/skills/reverse-engineer-anything/) remains under its
[MIT license](.agents/skills/reverse-engineer-anything.LICENSE), © 2026 morluto.
Dependencies retain their own licenses.
