//! Embedded assets: our own illustration plus the Lucide icons the interface
//! uses, vendored from `gpui-component-assets` at the rev `Cargo.toml` pinned so
//! both interfaces draw the same art.

use std::borrow::Cow;

/// Everything under `assets/icons`, compiled into the binary.
#[derive(rust_embed::RustEmbed)]
#[folder = "assets"]
#[include = "icons/**/*.svg"]
struct Local;

/// Reads an embedded asset by path, e.g. `icons/play.svg`.
pub fn load(path: &str) -> Option<Cow<'static, [u8]>> {
    Local::get(path).map(|f| f.data)
}

/// Serves our assets first, then falls back to gpui-component's bundled icon set
/// so `IconName::*` keeps working. gpui-component's loader returns Err (not
/// Ok(None)) on a miss, so ours must check membership before delegating.
#[cfg(feature = "legacy")]
pub struct AppAssets;

#[cfg(feature = "legacy")]
impl gpui::AssetSource for AppAssets {
    fn load(&self, path: &str) -> gpui::Result<Option<Cow<'static, [u8]>>> {
        if path.is_empty() {
            return Ok(None);
        }
        match Local::get(path) {
            Some(f) => Ok(Some(f.data)),
            None => gpui_component_assets::Assets.load(path),
        }
    }

    fn list(&self, path: &str) -> gpui::Result<Vec<gpui::SharedString>> {
        let mut out = gpui_component_assets::Assets.list(path)?;
        out.extend(
            Local::iter()
                .filter(|p| p.starts_with(path))
                .map(|p| gpui::SharedString::from(p.to_string())),
        );
        Ok(out)
    }
}
