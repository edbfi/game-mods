# Agent instructions

Mods for games, one folder per game and one subfolder per mod: `<game>/<mod>/`, both kebab-case.
CI comes from [edbfi-ci](https://github.com/edbfi/edbfi-ci); its rules apply here.

## Rules

- A mod is a folder with a `README.md` and a `justfile` with `test`, `build` and `package <version>`.
  `package` writes `dist/<game>-<mod>-<version>.zip`. Adding a mod needs no change outside its folder.
- Never commit game files, saves, captures or binaries. Build output goes in the mod's ignored `build/` and `dist/`.
- Conventional Commits scoped `<game>/<mod>` (e.g. `fix(metro-2033-redux/trainer): …`), with a DCO sign-off
  (`git commit -s`), through a PR.
- New source files carry `SPDX-License-Identifier: AGPL-3.0-only`.
- No PowerShell: scripts are POSIX sh or bash, run through `just`.
- Docs hold only what the code can't say. No reports in the repo: evidence goes in the PR body.
- Releases are tagged `<game>/<mod>/v<semver>`.
