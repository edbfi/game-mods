// SPDX-License-Identifier: AGPL-3.0-only
//! Safe wrappers over the Win32 calls the trainer needs. This is the only module with `unsafe` code; every
//! `unsafe` block states why it is sound.

pub(crate) mod fs;
