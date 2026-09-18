//! Siglus BG stage: g00 decoding + Siglus-like resource lookup + wgpu rendering.
//!
//! Code comments are intentionally in English.

pub mod app_path;
pub mod platform_time;

pub mod assets;
pub mod audio;
pub mod emote;
#[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
pub mod emote_key;
pub mod image_manager;
pub mod ime;
pub mod layer;
pub mod mesh3d;
pub mod movie;
pub mod original_save;
pub mod render_math;
pub mod resource;
pub mod runtime;
pub mod text_render;
#[cfg(target_arch = "wasm32")]
pub mod wasm_entry;
#[cfg(target_arch = "wasm32")]
pub mod wasm_vfs;

pub mod elm_code;

pub mod scene_stream;
pub mod vm;

// Re-export the format-first asset crate so higher layers (VM/app) can share
// parsers/decoders without wiring a second direct dependency.
pub use siglus_assets as formats;

pub mod render;

pub mod input;

#[cfg(target_os = "android")]
pub mod android_host;
#[cfg(any(target_os = "macos", target_os = "windows", target_os = "linux"))]
pub mod desktop_chihaya_bench;
#[cfg(any(target_os = "macos", target_os = "windows", target_os = "linux"))]
pub mod desktop_messagebox;
#[cfg(any(target_os = "macos", target_os = "windows", target_os = "linux"))]
pub mod desktop_twitter;
pub mod host;
#[cfg(target_os = "ios")]
pub mod ios_host;
#[cfg(any(target_os = "macos", target_os = "windows", target_os = "linux"))]
pub mod pump_host;

pub mod display_ffi;

#[cfg(any(target_os = "macos", target_os = "windows", target_os = "linux"))]
pub mod desktop_config;
#[cfg(any(target_os = "macos", target_os = "windows", target_os = "linux"))]
pub mod desktop_icon;
