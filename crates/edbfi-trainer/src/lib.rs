// SPDX-License-Identifier: AGPL-3.0-only
//! Shared trainer core for edbfi game mods.
//!
//! A game supplies a validated static profile. This crate finds and identifies the game process, installs
//! the profile's hooks under a thread freeze, publishes the control word and restores every byte it changed.
//! Portable modules forbid `unsafe`; all Win32 calls live in one `win32` module.
#![deny(unsafe_code)]
