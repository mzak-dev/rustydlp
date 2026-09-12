//! The dark palette, transcribed from gpui-component's `default-theme.json` at
//! the rev `Cargo.toml` pinned, resolved through its Tailwind-style
//! `default-colors.json` index.
//!
//! Dark only, deliberately: the app hardcoded `ThemeMode::Dark` at startup and
//! never read the mode again, so a light palette would be dead weight.
//!
//! `list_hover` and `radius` are NOT in that JSON — they fall out of
//! gpui-component's own defaults. They are marked below and are the two values
//! to confirm against the running legacy build during the parity pass.

use super::color::{Rgba, rgb};
use super::units::{Pixels, px};

pub struct Theme {
    pub background: Rgba,
    pub foreground: Rgba,
    pub muted: Rgba,
    pub muted_foreground: Rgba,
    pub border: Rgba,
    pub danger: Rgba,
    pub danger_foreground: Rgba,
    pub sidebar: Rgba,
    pub sidebar_foreground: Rgba,
    pub sidebar_border: Rgba,
    pub sidebar_accent: Rgba,
    pub sidebar_accent_foreground: Rgba,
    pub list_hover: Rgba,
    pub progress_bar: Rgba,
    pub radius: Pixels,
}

/// The palette entries the theme resolves to, kept named so the mapping back to
/// `default-theme.json` stays legible.
mod palette {
    use super::super::color::{Rgba, rgb};
    pub const NEUTRAL_950: Rgba = rgb(0x0a0a0a);
    pub const NEUTRAL_800: Rgba = rgb(0x262626);
    pub const NEUTRAL_400: Rgba = rgb(0xa3a3a3);
    pub const NEUTRAL_50: Rgba = rgb(0xfafafa);
    pub const RED_400: Rgba = rgb(0xf87171);
    pub const RED_600: Rgba = rgb(0xdc2626);
}

pub const DARK: Theme = Theme {
    background: palette::NEUTRAL_950,
    foreground: palette::NEUTRAL_50,
    muted: palette::NEUTRAL_800,
    muted_foreground: palette::NEUTRAL_400,
    border: palette::NEUTRAL_800,
    danger: palette::RED_400,
    danger_foreground: palette::RED_600,
    // Spelled as literals in the JSON rather than as palette references.
    sidebar: rgb(0x0a0a0a),
    sidebar_foreground: rgb(0xf5f5f5),
    sidebar_border: rgb(0x262626),
    sidebar_accent: rgb(0x262626),
    sidebar_accent_foreground: rgb(0xf5f5f5),
    // UNCONFIRMED: absent from default-theme.json. Sample from the running
    // legacy build before signing off parity.
    list_hover: rgb(0x1f1f1f),
    progress_bar: rgb(0xf5f5f5),
    // UNCONFIRMED, as above. gpui-component's default radius.
    radius: px(4.),
};

/// The active theme. A function rather than a constant so that the call sites
/// read like `theme().muted_foreground`, close to the `cx.theme()` they replace.
pub fn theme() -> &'static Theme {
    &DARK
}

/// gpui-component's base type scale: `font_family = ".SystemUIFont"`,
/// `font_size = px(16.)`.
///
/// `.SystemUIFont` is gpui's per-platform alias; on Windows it resolves through
/// DirectWrite to the system UI font. UNCONFIRMED which family that actually
/// is — Segoe UI is the strong prior. Confirm against the legacy build before
/// configuring the font database, because every text metric depends on it.
pub const BASE_FONT_SIZE: Pixels = px(16.);
pub const BASE_LINE_HEIGHT: f32 = 1.25;
