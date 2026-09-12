//! Embedded assets: our own illustration plus the Lucide icons the interface
//! uses.
//!
//! The Lucide set was vendored out of `gpui-component-assets` while the gpui
//! interface still existed, so the replacement draws the same art rather than
//! art that merely looks similar.
//!
//! A plain `include_bytes!` table rather than `rust-embed`: the asset list is
//! eleven known files that change about once a year, and the macro crate cost 28
//! dependencies to walk a directory at compile time. `include_bytes!` is in the
//! language, and a missing file becomes a compile error instead of a `None` at
//! runtime.

/// Every embedded asset, keyed by the path the interface asks for.
static ASSETS: &[(&str, &[u8])] = &[
    ("icons/chevron-left.svg", include_bytes!("../assets/icons/chevron-left.svg")),
    ("icons/chevron-right.svg", include_bytes!("../assets/icons/chevron-right.svg")),
    ("icons/close.svg", include_bytes!("../assets/icons/close.svg")),
    ("icons/empty-downloads.svg", include_bytes!("../assets/icons/empty-downloads.svg")),
    ("icons/folder.svg", include_bytes!("../assets/icons/folder.svg")),
    ("icons/maximize.svg", include_bytes!("../assets/icons/maximize.svg")),
    ("icons/minimize.svg", include_bytes!("../assets/icons/minimize.svg")),
    ("icons/pause.svg", include_bytes!("../assets/icons/pause.svg")),
    ("icons/play.svg", include_bytes!("../assets/icons/play.svg")),
    ("icons/plus.svg", include_bytes!("../assets/icons/plus.svg")),
    ("icons/replace.svg", include_bytes!("../assets/icons/replace.svg")),
    ("icons/restore.svg", include_bytes!("../assets/icons/restore.svg")),
    ("icons/rustydlp-icon.svg", include_bytes!("../assets/icons/rustydlp-icon.svg")),
    ("icons/rustydlp-logo.svg", include_bytes!("../assets/icons/rustydlp-logo.svg")),
    ("icons/settings.svg", include_bytes!("../assets/icons/settings.svg")),
    ("icons/volume-muted.svg", include_bytes!("../assets/icons/volume-muted.svg")),
    ("icons/volume.svg", include_bytes!("../assets/icons/volume.svg")),
];

/// Reads an embedded asset by path, e.g. `icons/play.svg`.
pub fn load(path: &str) -> Option<&'static [u8]> {
    ASSETS
        .iter()
        .find(|(name, _)| *name == path)
        .map(|(_, bytes)| *bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every icon the interface names has to actually be embedded, and the table
    /// is what makes that a compile-time guarantee rather than a blank square at
    /// runtime. This checks the other half: that the names match what is asked
    /// for.
    #[test]
    fn every_named_icon_resolves() {
        for name in crate::widget::icon::ALL_ICON_PATHS {
            assert!(load(name).is_some(), "{name} is not embedded");
        }
        assert!(load("icons/empty-downloads.svg").is_some());
        assert!(load("icons/volume.svg").is_some());
        assert!(load("icons/volume-muted.svg").is_some());
        assert!(load("icons/nope.svg").is_none());
    }
}
