# Changelog

## [0.6.0](https://github.com/mzak-dev/rustydlp/compare/rustydlp-v0.5.0...rustydlp-v0.6.0) (2026-09-13)


### Features

* **ci:** automate version bumps and GitHub releases from Conventional Commits ([cdb8425](https://github.com/mzak-dev/rustydlp/commit/cdb8425ec65a6ab67b5457e2ce44d2900152020e))
* Fix broken thumbnails, slider, truncation, and playback stutter/desync; add custom window chrome and app branding ([5c5dad8](https://github.com/mzak-dev/rustydlp/commit/5c5dad8def3588d72aa24b79cc94b6d475d06914))
* **render:** D3D12 backend, smoother playback, and draggable player sliders ([1ccf139](https://github.com/mzak-dev/rustydlp/commit/1ccf139a681f20d279ee0d4e9e96a6581c78f609))
* **render:** render the interface through D3D12 ([046394e](https://github.com/mzak-dev/rustydlp/commit/046394e372fe5a3ca20e4fac24c9aa7c6f7b4678))
* replace the sidebar with a Home library grid and a popover detail view ([57e9abb](https://github.com/mzak-dev/rustydlp/commit/57e9abba2d4adad8d4778f4ccb31a3709ad678e1))
* **ui:** add hover-enter/leave events to the render engine ([bdd8b6d](https://github.com/mzak-dev/rustydlp/commit/bdd8b6dc695cdb2f9bc27287bb2e3bda7f9c6b5f))
* **ui:** redesign the settings page and relocate nav actions ([3f9dc75](https://github.com/mzak-dev/rustydlp/commit/3f9dc75c00b8797d3d6e572d3a05ba1315529fd0))


### Bug Fixes

* **player:** make slider thumbs follow the pointer while dragging ([3cbd3fe](https://github.com/mzak-dev/rustydlp/commit/3cbd3fec6c58cf568f461ed1853da18e1a1e2c0d))


### Performance Improvements

* **player:** cap decode resolution and skip the per-frame zero-fill ([942019a](https://github.com/mzak-dev/rustydlp/commit/942019ab11118d3f0f9d5c80d1277fd8293659c8))
