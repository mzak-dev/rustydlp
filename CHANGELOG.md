# Changelog

## [0.7.0](https://github.com/mzak-dev/rustydlp/compare/v0.6.0...v0.7.0) (2026-09-14)


### Features

* **ui:** cross-fade between screens and add a way back out of Settings ([b521c40](https://github.com/mzak-dev/rustydlp/commit/b521c40f8eb3b2fe5edbc7efd28389cc31da4e24))
* **ui:** morph the library popover out of the tile that opened it ([1431a90](https://github.com/mzak-dev/rustydlp/commit/1431a901c0660d2e5f56a689ef861e6e7f1b9733))
* **ui:** rebuild the download and convert dialogs ([ea93836](https://github.com/mzak-dev/rustydlp/commit/ea938368ccd95e9c6342429103c7023a7f0c88ae))
* **ui:** show a loading bar while a file opens, and stop the download indicator stuttering ([9b9a4b0](https://github.com/mzak-dev/rustydlp/commit/9b9a4b01506e9c41a38f5ac18084e02d71795b68))
* **ui:** show what a job is actually doing, under the detail pane's files ([caa8381](https://github.com/mzak-dev/rustydlp/commit/caa83814487ecc77e46abf0a4a74d1a5682e4a92))

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
