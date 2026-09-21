// SPDX-License-Identifier: GPL-3.0-only
// SPDX-FileCopyrightText: 2026 Corey T. White

//! Design tokens: every color the GUI uses, in one place.
//! Dark theme first; the base widget styling comes from `iced::Theme::Dark`,
//! these tokens cover the custom canvas drawing and accents.

use iced::Color;

pub const ACCENT: Color = Color::from_rgb(0.36, 0.68, 0.89);
pub const ACCENT_DIM: Color = Color::from_rgb(0.22, 0.40, 0.53);
pub const TEXT: Color = Color::from_rgb(0.90, 0.90, 0.92);
pub const TEXT_DIM: Color = Color::from_rgb(0.55, 0.56, 0.60);
pub const PANEL: Color = Color::from_rgb(0.12, 0.12, 0.14);
pub const TRACK: Color = Color::from_rgb(0.25, 0.25, 0.28);
pub const PAD_OFF: Color = Color::from_rgb(0.20, 0.20, 0.23);
pub const PAD_ON: Color = Color::from_rgb(0.95, 0.55, 0.25);
pub const WARN: Color = Color::from_rgb(0.90, 0.45, 0.35);
pub const LOOP_REC: Color = Color::from_rgb(0.90, 0.28, 0.30);
pub const LOOP_PLAY: Color = Color::from_rgb(0.30, 0.72, 0.42);
pub const LOOP_STOP: Color = Color::from_rgb(0.85, 0.66, 0.25);
/// An empty loop slot, mirroring the hardware's idle pad wash.
pub const SLOT_EMPTY: Color = Color::from_rgb(0.10, 0.20, 0.28);
