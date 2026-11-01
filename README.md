# game-mods

Mods for games I play. One folder per game, one subfolder per mod.

| Game | Mod | Kind | Status |
| --- | --- | --- | --- |
| Metro 2033 Redux | [trainer](metro-2033-redux/trainer/) | INI-configured Windows trainer | Experimental |
| Dawn of War – Definitive Edition | [munitorum-logistics](dawn-of-war-de/munitorum-logistics/) | Always-on wincondition script | Playable |

Download a mod from [Releases](https://github.com/edbfi/game-mods/releases). Its README says how to install it.
Release assets are scanned with VirusTotal; a memory-patching trainer can still trip heuristic detections.

## Develop

Each mod has a `justfile` with `test`, `build` and `package <version>`. From the repo root, `just test` and
`just build` run them for every mod.

## License

[AGPL-3.0-only](LICENSE), © edbfi.
