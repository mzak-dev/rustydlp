//! Everything that is not the user interface: the job/preset model, the
//! turso-backed store, the yt-dlp and ffmpeg process runners, the argument
//! builder, and the native player's decode threads.
//!
//! Nothing in here may depend on the UI layer or on any GUI crate. That is
//! what makes the interface replaceable: the modules below talk to the UI
//! only through plain data and `futures_channel` receivers, and their tests
//! run without a window, a GPU, or a renderer.

pub mod model;
pub mod player;
pub mod runner;
pub mod store;
pub mod ytdlp;
