// SPDX-License-Identifier: AGPL-3.0-only
//! Shared trainer core for edbfi game mods: process identity, verified hook caves, controls and settings.
//!
//! Win32 `unsafe` lives only in the private `win32` module; everything else is safe code.
#![deny(unsafe_code)]

pub mod addr;
pub mod aob;
pub mod cave;
pub mod chord;
pub mod control;
pub mod identity;
pub mod input;
pub mod journal;
pub mod lifecycle;
pub mod location;
pub mod pe;
pub mod profile;
pub mod recovery;
pub mod settings;
#[cfg(test)]
mod testdir;
#[cfg(windows)]
#[allow(unsafe_code)]
mod win32;
