//! The engine as seen from Flutter. `api` is what Dart calls; `frb_generated`
//! is written by `flutter_rust_bridge_codegen`.

#[cfg(target_os = "android")]
mod android;
pub mod api;
mod frb_generated;
mod state;
