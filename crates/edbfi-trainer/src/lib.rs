// SPDX-License-Identifier: AGPL-3.0-only
//! Shared trainer core for edbfi game mods: process identity, verified hooks, controls and settings.
//!
//! All Win32 `unsafe` will live in a single `win32` module; everything else stays safe code.
#![deny(unsafe_code)]

pub mod addr;
pub mod aob;
pub mod chord;
pub mod control;
pub mod identity;
pub mod pe;
pub mod profile;
pub mod recovery;
