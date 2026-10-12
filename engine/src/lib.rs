//! Everything except the interface: markup model, audio reading and playback,
//! the library, processing and export. No GTK here, so the desktop app and the
//! Android app share it.

pub mod audio;
pub mod core;
pub mod dsp;
pub mod export;
pub mod lang;
pub mod recording;
pub mod remote;
pub mod store;
pub mod sync;
pub mod vfs;
