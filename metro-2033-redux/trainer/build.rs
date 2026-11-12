// SPDX-License-Identifier: AGPL-3.0-or-later
//! Embeds the Windows manifest and version resource.
//!
//! `EDBFI_VERSION` (`X.Y.Z`, default `0.0.0`) and `EDBFI_COMMIT` (hex, default empty) come from packaging and CI,
//! never from git. The resource script is generated so that only validated numbers and hex reach it.

use std::env;
use std::fmt::Write as _;
use std::fs;
use std::path::PathBuf;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("cargo:rerun-if-env-changed=EDBFI_VERSION");
    println!("cargo:rerun-if-env-changed=EDBFI_COMMIT");
    println!("cargo:rerun-if-changed=resources/trainer.manifest");

    let version = env::var("EDBFI_VERSION").unwrap_or_else(|_| "0.0.0".to_owned());
    let [major, minor, patch] = parse_version(&version).ok_or_else(|| {
        format!("EDBFI_VERSION must be X.Y.Z without leading zeros, got {version:?}")
    })?;
    let commit = env::var("EDBFI_COMMIT").unwrap_or_default();
    if commit.len() > 40 || !commit.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(format!("EDBFI_COMMIT must be at most 40 hex digits, got {commit:?}").into());
    }
    println!("cargo:rustc-env=EDBFI_VERSION={version}");
    println!("cargo:rustc-env=EDBFI_COMMIT={commit}");

    if env::var("CARGO_CFG_TARGET_OS")? != "windows" {
        return Ok(());
    }
    let manifest =
        PathBuf::from(env::var("CARGO_MANIFEST_DIR")?).join("resources/trainer.manifest");
    let manifest = manifest
        .to_str()
        .ok_or("manifest path is not UTF-8")?
        .replace('\\', "/");
    if manifest.contains('"') {
        return Err("manifest path contains a quote".into());
    }
    let product = if commit.is_empty() {
        version.clone()
    } else {
        format!("{version}+{commit}")
    };

    let mut rc = String::new();
    writeln!(rc, "1 24 \"{manifest}\"")?;
    writeln!(rc, "1 VERSIONINFO")?;
    writeln!(rc, "FILEVERSION {major},{minor},{patch},0")?;
    writeln!(rc, "PRODUCTVERSION {major},{minor},{patch},0")?;
    writeln!(rc, "FILEOS 0x40004")?;
    writeln!(rc, "FILETYPE 0x1")?;
    writeln!(rc, "BEGIN")?;
    writeln!(rc, "  BLOCK \"StringFileInfo\"")?;
    writeln!(rc, "  BEGIN")?;
    writeln!(rc, "    BLOCK \"040904B0\"")?;
    writeln!(rc, "    BEGIN")?;
    writeln!(rc, "      VALUE \"CompanyName\", \"edbfi\"")?;
    writeln!(
        rc,
        "      VALUE \"FileDescription\", \"Metro 2033 Redux trainer\""
    )?;
    writeln!(rc, "      VALUE \"FileVersion\", \"{version}\"")?;
    writeln!(rc, "      VALUE \"InternalName\", \"metro-trainer\"")?;
    writeln!(rc, "      VALUE \"LegalCopyright\", \"AGPL-3.0-or-later\"")?;
    writeln!(
        rc,
        "      VALUE \"OriginalFilename\", \"metro-trainer.exe\""
    )?;
    writeln!(rc, "      VALUE \"ProductName\", \"edbfi game-mods\"")?;
    writeln!(rc, "      VALUE \"ProductVersion\", \"{product}\"")?;
    writeln!(rc, "    END")?;
    writeln!(rc, "  END")?;
    writeln!(rc, "  BLOCK \"VarFileInfo\"")?;
    writeln!(rc, "  BEGIN")?;
    writeln!(rc, "    VALUE \"Translation\", 0x409, 1200")?;
    writeln!(rc, "  END")?;
    writeln!(rc, "END")?;

    let script = PathBuf::from(env::var("OUT_DIR")?).join("trainer.rc");
    fs::write(&script, rc)?;
    embed_resource::compile(&script, embed_resource::NONE)
        .manifest_required()
        .map_err(|error| format!("failed to embed Windows resources: {error}"))?;
    Ok(())
}

/// Parses `X.Y.Z` with each part a `u16` without leading zeros.
fn parse_version(version: &str) -> Option<[u16; 3]> {
    let mut parts = version.split('.').map(|part| {
        let canonical =
            part == "0" || (!part.starts_with('0') && part.bytes().all(|b| b.is_ascii_digit()));
        if canonical { part.parse().ok() } else { None }
    });
    let parsed = [parts.next()??, parts.next()??, parts.next()??];
    parts.next().is_none().then_some(parsed)
}
