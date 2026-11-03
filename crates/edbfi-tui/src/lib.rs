// SPDX-License-Identifier: AGPL-3.0-only
//! Terminal dashboard shared by edbfi game trainers.
//!
//! [`app::App`] is a pure reducer over the runtime's phase, acknowledgements and user input. Drawing `&App`
//! into a ratatui buffer is deterministic, and [`plain::Report`] prints the same model as plain ASCII lines
//! for redirected output or a lost console. Nothing here reads a terminal or touches a process.
#![forbid(unsafe_code)]

pub mod app;
pub mod driver;
pub mod layout;
pub mod outcome;
pub mod plain;
pub mod text;
pub mod theme;
mod view;
mod widgets;
