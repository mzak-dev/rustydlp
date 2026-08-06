use gpui::{AssetSource, Result, SharedString};
use std::borrow::Cow;

/// Our own SVGs (empty-state illustration, etc).
#[derive(rust_embed::RustEmbed)]
#[folder = "assets"]
#[include = "icons/**/*.svg"]
struct Local;

/// Serves our assets first, then falls back to gpui-component's bundled icon set
/// so `IconName::*` keeps working. gpui-component's loader returns Err (not
/// Ok(None)) on a miss, so ours must check membership before delegating.
pub struct AppAssets;

impl AssetSource for AppAssets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        if path.is_empty() {
            return Ok(None);
        }
        match Local::get(path) {
            Some(f) => Ok(Some(f.data)),
            None => gpui_component_assets::Assets.load(path),
        }
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        let mut out = gpui_component_assets::Assets.list(path)?;
        out.extend(
            Local::iter()
                .filter(|p| p.starts_with(path))
                .map(|p| SharedString::from(p.to_string())),
        );
        Ok(out)
    }
}
