//! SVG rendered to an alpha mask, then tinted.
//!
//! This is what gpui did, and `assets/icons/empty-downloads.svg` says so in its
//! own comment: *"GPUI rasterises this to an alpha mask and tints it with
//! text_color, so the literal stroke colour is irrelevant; only opacity carries
//! depth."* Both call sites in the old `app.rs` rely on it, and the vendored
//! Lucide icons are `stroke="currentColor"` for the same reason.
//!
//! resvg rather than skia-safe's `svg` feature: prebuilt Skia archives are keyed
//! on the feature combo, and none published covers one including `svg`, so
//! enabling it forces a full source build.

use std::collections::HashMap;

use resvg::{tiny_skia, usvg};

use super::color::Rgba;

/// Parses and rasterizes icons, caching both.
#[derive(Default)]
pub struct SvgRenderer {
    /// `None` records a failed parse so a broken path is not retried per frame.
    trees: HashMap<String, Option<usvg::Tree>>,
    /// Alpha masks by path and pixel size. Icons are drawn at a handful of
    /// sizes, so this stays small and stops re-rasterizing every frame.
    masks: HashMap<(String, u32, u32), Vec<u8>>,
}

impl SvgRenderer {
    pub fn new() -> Self {
        Self::default()
    }

    fn tree(&mut self, path: &str) -> Option<&usvg::Tree> {
        if !self.trees.contains_key(path) {
            let parsed = crate::assets::load(path)
                .and_then(|bytes| usvg::Tree::from_data(&bytes, &usvg::Options::default()).ok());
            self.trees.insert(path.to_string(), parsed);
        }
        self.trees.get(path).and_then(|t| t.as_ref())
    }

    /// The icon's coverage at `width` x `height`, one byte per pixel.
    fn mask(&mut self, path: &str, width: u32, height: u32) -> Option<&[u8]> {
        let key = (path.to_string(), width, height);
        if !self.masks.contains_key(&key) {
            let mask = self.render_mask(path, width, height)?;
            self.masks.insert(key.clone(), mask);
        }
        self.masks.get(&key).map(|m| m.as_slice())
    }

    fn render_mask(&mut self, path: &str, width: u32, height: u32) -> Option<Vec<u8>> {
        let tree = self.tree(path)?;
        let size = tree.size();
        if size.width() <= 0.0 || size.height() <= 0.0 {
            return None;
        }
        let mut pixmap = tiny_skia::Pixmap::new(width, height)?;
        let transform = tiny_skia::Transform::from_scale(
            width as f32 / size.width(),
            height as f32 / size.height(),
        );
        resvg::render(tree, transform, &mut pixmap.as_mut());
        // Only the alpha channel is wanted. tiny-skia's buffer is
        // premultiplied, which is irrelevant here: premultiplying does not
        // change alpha itself.
        Some(pixmap.data().chunks_exact(4).map(|px| px[3]).collect())
    }

    /// The icon at `width` x `height` as straight RGBA, every covered pixel set
    /// to `tint` and the mask carried in alpha.
    pub fn tinted_rgba(
        &mut self,
        path: &str,
        width: u32,
        height: u32,
        tint: Rgba,
    ) -> Option<Vec<u8>> {
        let (r, g, b) = (
            (tint.r * 255.0).round() as u8,
            (tint.g * 255.0).round() as u8,
            (tint.b * 255.0).round() as u8,
        );
        let mask = self.mask(path, width, height)?;
        let mut out = Vec::with_capacity(mask.len() * 4);
        for &coverage in mask {
            out.extend_from_slice(&[r, g, b, (coverage as f32 * tint.a).round() as u8]);
        }
        Some(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::color::rgb;

    /// A vendored Lucide icon has to parse, rasterize to some coverage, and come
    /// back in the tint colour rather than its own `currentColor` black.
    #[test]
    fn an_icon_rasterizes_to_a_mask_and_takes_the_tint() {
        let mut r = SvgRenderer::new();
        let tint = rgb(0xa3a3a3);
        let rgba = r
            .tinted_rgba("icons/play.svg", 24, 24, tint)
            .expect("play.svg should be embedded and parseable");
        assert_eq!(rgba.len(), 24 * 24 * 4);

        let covered: Vec<&[u8]> =
            rgba.chunks_exact(4).filter(|px| px[3] > 0).collect();
        assert!(!covered.is_empty(), "the icon drew nothing");
        for px in covered {
            assert_eq!(
                (px[0], px[1], px[2]),
                (0xa3, 0xa3, 0xa3),
                "covered pixels must carry the tint, not the SVG's own colour",
            );
        }
    }

    /// Our own illustration uses per-shape `opacity` to carry depth, and that has
    /// to survive into the mask as partial coverage rather than being flattened.
    #[test]
    fn per_shape_opacity_becomes_partial_coverage() {
        let mut r = SvgRenderer::new();
        let rgba = r
            .tinted_rgba("icons/empty-downloads.svg", 128, 96, rgb(0xffffff))
            .expect("embedded illustration");
        let alphas: Vec<u8> = rgba.chunks_exact(4).map(|px| px[3]).collect();
        assert!(alphas.iter().any(|&a| a > 0 && a < 255), "expected partial coverage");
    }

    /// A missing path must be recorded, not retried, and must not panic.
    #[test]
    fn a_missing_icon_is_cached_as_a_failure() {
        let mut r = SvgRenderer::new();
        assert!(r.tinted_rgba("icons/nope.svg", 16, 16, rgb(0xffffff)).is_none());
        assert!(r.trees.contains_key("icons/nope.svg"), "the miss is remembered");
        assert!(r.tinted_rgba("icons/nope.svg", 16, 16, rgb(0xffffff)).is_none());
    }
}
