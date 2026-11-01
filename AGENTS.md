# Agent instructions

Mods for games, one folder per game and one subfolder per mod: `<game>/<mod>/`, both kebab-case.
CI comes from [edbfi-ci](https://github.com/edbfi/edbfi-ci); its rules apply here.

## Rules

- A mod is a folder with a `README.md` and a `justfile` with `test`, `build` and `package <version>`.
  `package` writes `dist/<game>-<mod>-<version>.zip`. Adding a mod needs no change outside its folder.
- Rust code is one Cargo workspace. Rust trainers live in `<game>/trainer` and join it automatically; other Rust
  mods add themselves to the root `Cargo.toml` members, and any Rust change may update the root `Cargo.lock`.
  Shared crates live in `crates/` and never hold a `justfile`.
- Never commit game files, saves, captures or binaries. Build output goes in the mod's ignored `build/` and `dist/`.
- Conventional Commits scoped `<game>/<mod>` (e.g. `fix(metro-2033-redux/trainer): …`), with a DCO sign-off
  (`git commit -s`), through a PR.
- New source files carry `SPDX-License-Identifier: AGPL-3.0-only`.
- No PowerShell: scripts are POSIX sh or bash, run through `just`.
- Docs hold only what the code can't say. No reports in the repo: evidence goes in the PR body.

## Versions and releases

Every merge to main ships as a nightly build (`.github/workflows/nightly.yml`). Release a mod only when a
version of it is worth keeping; nightlies cover the changes in between.

- Each mod has its own [SemVer](https://semver.org/) version, `X.Y.Z` with no leading zeros.
- Release by publishing a GitHub release from main, tagged and titled `<game>/<mod>/vX.Y.Z`
  (e.g. `metro-2033-redux/trainer/v0.1.0`). Never move, delete or reuse a tag; fix a bad release with a new one.
- The first release is `0.1.0`. After that, bump from the mod's commits since its last tag:
  breaking → major (minor while the version is `0.y.z`), `feat` → minor, `fix` or `perf` → patch.
- A change is breaking when users must act on it: a renamed or removed setting, hotkey or file, a different
  install layout, or a dropped game version. Mark it with `!` in the commit header.
