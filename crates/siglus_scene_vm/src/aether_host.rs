//! AetherKiri embedded-host FFI.
//!
//! Drives SiglusEngine without owning a window or event loop: the renderer is
//! built headless via [`Renderer::new_offscreen`], frames are read back over
//! the CPU, and input arrives through explicit calls from the host. This is
//! the integration surface behind AetherKiri's `bridge/siglus_runtime`
//! provider; the C++ side mirrors these entry points in `siglus_ffi.h`.
//!
//! Error codes mirror `engine_result_t` in engine_api.h so the C++ provider
//! can pass them through unchanged.

use std::ffi::{c_char, c_void, CStr, CString};
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::host::{cstr_required, SiglusHost, SiglusHostConfig, SiglusNativeMessageBoxCallback};
use crate::render::Renderer;
use crate::runtime::input::VmMouseButton;

/// Debug trace sink: <game_root>/aether_debug.log. Set by siglus_ak_open so
/// embedded-host bring-up can be followed from `adb shell cat` even when the
/// Rust log stack has no logcat backend.
static TRACE_LOG_PATH: Mutex<Option<PathBuf>> = Mutex::new(None);

fn trace_log(message: &str) {
    let Ok(path) = TRACE_LOG_PATH.lock() else {
        return;
    };
    let Some(path) = path.as_ref() else {
        return;
    };
    use std::io::Write;
    if let Ok(mut file) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0);
        let _ = writeln!(file, "[{now}] {message}");
    }
}


pub const SIGLUS_AK_FFI_API_VERSION: u32 = 0x0001_0000;

#[repr(C)]
#[derive(Default)]
pub struct SiglusTextInputState {
    pub active: u32,
    pub x: i32,
    pub y: i32,
    pub text_bytes: u32,
    pub selection_start: i32,
    pub selection_end: i32,
}

#[no_mangle]
pub unsafe extern "C" fn siglus_ak_get_text_input_state(
    handle: *mut SiglusAetherHost, output: *mut SiglusTextInputState,
) -> i32 {
    let (Some(handle), Some(output)) = (handle.as_mut(), output.as_mut()) else { return SIGLUS_AK_INVALID_ARGUMENT; };
    *output = SiglusTextInputState::default();
    handle.with_inner(|host| {
        let ctx = &host.vm_mut().ctx;
        let Some((x, y, _, height)) = ctx.focused_editbox_ime_area() else { return; };
        let Some((form, idx)) = ctx.globals.focused_editbox else { return; };
        let Some(eb) = ctx.globals.editbox_lists.get(&form).and_then(|list| list.boxes.get(idx)) else { return; };
        let (start, end) = eb.selection_range().unwrap_or((eb.cursor_pos, eb.cursor_pos));
        let scalar_offset = |byte: usize| eb.text.get(..byte).map(|s| s.chars().count() as i32).unwrap_or(0);
        *output = SiglusTextInputState { active: 1, x, y: y.saturating_add(height),
            text_bytes: eb.text.len() as u32, selection_start: scalar_offset(start), selection_end: scalar_offset(end) };
    })
}

#[no_mangle]
pub unsafe extern "C" fn siglus_ak_copy_text_input_text(
    handle: *mut SiglusAetherHost, output: *mut c_char, size: usize, written: *mut u32,
) -> i32 {
    let Some(handle) = handle.as_mut() else { return SIGLUS_AK_INVALID_ARGUMENT; };
    if output.is_null() || size == 0 || written.is_null() { return SIGLUS_AK_INVALID_ARGUMENT; }
    *output = 0;
    *written = 0;
    let mut fits = true;
    let result = handle.with_inner(|host| {
        let ctx = &host.vm_mut().ctx;
        let Some((form, idx)) = ctx.globals.focused_editbox else { return; };
        let Some(eb) = ctx.globals.editbox_lists.get(&form).and_then(|list| list.boxes.get(idx)) else { return; };
        if eb.text.len() >= size { fits = false; return; }
        std::ptr::copy_nonoverlapping(eb.text.as_ptr(), output.cast(), eb.text.len());
        *output.add(eb.text.len()) = 0;
        *written = eb.text.len() as u32;
    });
    if !fits { SIGLUS_AK_INVALID_ARGUMENT } else { result }
}

/// Preedit cursor offsets use Unicode scalars at the public host boundary.
#[no_mangle]
pub unsafe extern "C" fn siglus_ak_ime_preedit(
    handle: *mut SiglusAetherHost, text: *const c_char, start: i32, length: i32,
) -> i32 {
    let Some(handle) = handle.as_mut() else { return SIGLUS_AK_INVALID_ARGUMENT; };
    if text.is_null() { return handle.with_inner(|host| host.ime_disabled()); }
    let text = match CStr::from_ptr(text).to_str() { Ok(text) => text, Err(_) => return SIGLUS_AK_INVALID_ARGUMENT };
    let offset = |n: usize| text.char_indices().nth(n).map(|(i, _)| i).unwrap_or(text.len());
    let cursor = (start >= 0).then(|| (offset(start as usize), offset(start.saturating_add(length.max(0)) as usize)));
    handle.with_inner(|host| host.ime_preedit(text, cursor))
}

const SIGLUS_AK_OK: i32 = 0;
const SIGLUS_AK_EXIT_REQUESTED: i32 = 1;
const SIGLUS_AK_INVALID_ARGUMENT: i32 = -1;
const SIGLUS_AK_INVALID_STATE: i32 = -2;
const SIGLUS_AK_NOT_SUPPORTED: i32 = -3;
const SIGLUS_AK_INTERNAL_ERROR: i32 = -5;

#[no_mangle]
pub unsafe extern "C" fn siglus_ak_joypad_button(handle: *mut SiglusAetherHost, button: u32, pressed: u32) -> i32 {
    let Some(handle) = handle.as_mut() else { return SIGLUS_AK_INVALID_ARGUMENT; };
    if button >= crate::runtime::input::JOYPAD_KEY_COUNT as u32 { return SIGLUS_AK_INVALID_ARGUMENT; }
    handle.with_inner(|host| host.joypad_button(button as usize, pressed != 0))
}

pub struct SiglusAetherHost {
    /// Set by [`siglus_ak_open`]; None until a game root was opened.
    inner: Option<SiglusHost>,
    scale_factor: f32,
    last_error: String,
    // Rust strings are not NUL-terminated; keep an owned C representation
    // alive for callers of siglus_ak_last_error, including the empty case.
    last_error_cstr: CString,
}

impl SiglusAetherHost {
    fn new(scale_factor: f32) -> Self {
        Self {
            inner: None,
            scale_factor: if scale_factor.is_finite() && scale_factor > 0.0 {
                scale_factor
            } else {
                1.0
            },
            last_error: String::new(),
            last_error_cstr: CString::default(),
        }
    }

    fn record_error(&mut self, err: anyhow::Error) -> i32 {
        self.last_error = format!("{err:#}");
        SIGLUS_AK_INTERNAL_ERROR
    }

    fn with_inner(&mut self, body: impl FnOnce(&mut SiglusHost)) -> i32 {
        match self.inner.as_mut() {
            Some(host) => {
                body(host);
                SIGLUS_AK_OK
            }
            None => {
                self.last_error = "no game is open".to_string();
                SIGLUS_AK_INVALID_STATE
            }
        }
    }
}

#[no_mangle]
pub extern "C" fn siglus_ak_ffi_api_version() -> u32 {
    SIGLUS_AK_FFI_API_VERSION
}

/// Creates an empty host shell. Call [`siglus_ak_open`] before any other call.
///
/// # Safety
/// Returns an owned handle; release it with exactly one [`siglus_ak_destroy`].
#[no_mangle]
pub unsafe extern "C" fn siglus_ak_create(scale_factor: f32) -> *mut SiglusAetherHost {
    Box::into_raw(Box::new(SiglusAetherHost::new(scale_factor)))
}

/// Builds the offscreen renderer and boots the scene VM for `game_root_utf8`.
/// Loading is synchronous and may take several seconds for large games.
///
/// # Safety
/// `handle` must come from [`siglus_ak_create`] and not be null;
/// `game_root_utf8` must be a valid NUL-terminated UTF-8 path.
#[no_mangle]
pub unsafe extern "C" fn siglus_ak_open(
    handle: *mut SiglusAetherHost,
    game_root_utf8: *const c_char,
    width: u32,
    height: u32,
) -> i32 {
    let handle = match handle.as_mut() {
        Some(h) => h,
        None => return SIGLUS_AK_INVALID_ARGUMENT,
    };
    if handle.inner.is_some() {
        handle.last_error = "a game is already open".to_string();
        return SIGLUS_AK_INVALID_STATE;
    }
    let game_root = match cstr_required(game_root_utf8, "game_root_utf8") {
        Ok(s) => s,
        Err(e) => {
            handle.last_error = format!("{e:#}");
            return SIGLUS_AK_INVALID_ARGUMENT;
        }
    };
    {
        let root = PathBuf::from(&game_root);
        if let Ok(mut guard) = TRACE_LOG_PATH.lock() {
            *guard = Some(root.join("aether_debug.log"));
        }
        trace_log(&format!(
            "=== siglus_ak_open begin ({game_root}) ==="
        ));
    }
    trace_log("renderer: new_offscreen starting");
    let result = (|| -> anyhow::Result<()> {
        let renderer = pollster::block_on(Renderer::new_offscreen(
            width.max(1),
            height.max(1),
            handle.scale_factor,
        ))?;
        trace_log("renderer: new_offscreen done");
        trace_log("host: new_with_renderer starting");
        let config = SiglusHostConfig::new(PathBuf::from(game_root));
        let host = pollster::block_on(SiglusHost::new_with_renderer(config, renderer))?;
        trace_log("host: new_with_renderer done");
        handle.inner = Some(host);
        Ok(())
    })();
    match result {
        Ok(()) => {
            trace_log("siglus_ak_open OK");
            SIGLUS_AK_OK
        }
        Err(e) => {
            trace_log(&format!("siglus_ak_open failed: {e:#}"));
            handle.record_error(e)
        }
    }
}

/// Tears down the open game (if any) but keeps the shell usable for another
/// [`siglus_ak_open`]. Safe to call when no game is open.
///
/// # Safety
/// `handle` must come from [`siglus_ak_create`] and not be null.
#[no_mangle]
pub unsafe extern "C" fn siglus_ak_close(handle: *mut SiglusAetherHost) {
    if let Some(h) = handle.as_mut() {
        h.inner = None;
        h.last_error.clear();
    }
}

/// Destroys the host shell. The handle is invalid after this call.
///
/// # Safety
/// Same contract as [`siglus_ak_close`]; must be called exactly once.
#[no_mangle]
pub unsafe extern "C" fn siglus_ak_destroy(handle: *mut SiglusAetherHost) {
    if !handle.is_null() {
        drop(Box::from_raw(handle));
    }
}

/// # Safety
/// `handle` must be a live handle returned by [`siglus_ak_create`].
#[no_mangle]
pub unsafe extern "C" fn siglus_ak_resize(
    handle: *mut SiglusAetherHost,
    width: u32,
    height: u32,
) -> i32 {
    let handle = match handle.as_mut() {
        Some(h) => h,
        None => return SIGLUS_AK_INVALID_ARGUMENT,
    };
    let scale_factor = handle.scale_factor;
    handle.with_inner(|host| {
        host.resize(width.max(1), height.max(1), scale_factor);
    })
}

/// Advances the simulation and renders one offscreen frame. Returns
/// [`SIGLUS_AK_OK`], [`SIGLUS_AK_EXIT_REQUESTED`] when the engine asked to
/// quit, or a negative error code.
///
/// # Safety
/// `handle` must be a live handle returned by [`siglus_ak_create`].
#[no_mangle]
pub unsafe extern "C" fn siglus_ak_step(handle: *mut SiglusAetherHost, dt_ms: u32) -> i32 {
    let handle = match handle.as_mut() {
        Some(h) => h,
        None => return SIGLUS_AK_INVALID_ARGUMENT,
    };
    let Some(host) = handle.inner.as_mut() else {
        handle.last_error = "no game is open".to_string();
        return SIGLUS_AK_INVALID_STATE;
    };
    trace_log(&format!("siglus_ak_step enter dt={dt_ms}"));
    match host.step(dt_ms.max(1)) {
        Ok(false) => {
            trace_log("siglus_ak_step OK");
            SIGLUS_AK_OK
        }
        Ok(true) => {
            trace_log("siglus_ak_step exit requested");
            SIGLUS_AK_EXIT_REQUESTED
        }
        Err(e) => {
            trace_log(&format!("siglus_ak_step failed: {e:#}"));
            handle.record_error(e)
        }
    }
}

/// Game-native `(width, height)` declared by the game's Gameexe
/// SCREEN_SIZE entry. [`SIGLUS_AK_NOT_SUPPORTED`] when unknown; embedded
/// hosts use it to cap the offscreen surface at native resolution and let
/// the presentation layer upscale.
///
/// # Safety
/// `handle` must be a live handle returned by [`siglus_ak_create`]; out
/// pointers must be writable when non-null.
#[no_mangle]
pub unsafe extern "C" fn siglus_ak_game_screen_size(
    handle: *mut SiglusAetherHost,
    out_width: *mut u32,
    out_height: *mut u32,
) -> i32 {
    let handle = match handle.as_mut() {
        Some(h) => h,
        None => return SIGLUS_AK_INVALID_ARGUMENT,
    };
    let Some(host) = handle.inner.as_ref() else {
        handle.last_error = "no game is open".to_string();
        return SIGLUS_AK_INVALID_STATE;
    };
    match host.gameexe_screen_size_pub() {
        Some((width, height)) => {
            if !out_width.is_null() {
                *out_width = width;
            }
            if !out_height.is_null() {
                *out_height = height;
            }
            SIGLUS_AK_OK
        }
        None => SIGLUS_AK_NOT_SUPPORTED,
    }
}

/// Writes the current frame geometry: `*out_width`, `*out_height`,
/// `*out_stride_bytes` (tightly packed RGBA rows). Zeroes on failure.
///
/// # Safety
/// All out pointers must be valid (non-null when the handle is live).
#[no_mangle]
pub unsafe extern "C" fn siglus_ak_get_frame_desc(
    handle: *mut SiglusAetherHost,
    out_width: *mut u32,
    out_height: *mut u32,
    out_stride_bytes: *mut u32,
) -> i32 {
    let handle = match handle.as_mut() {
        Some(h) => h,
        None => return SIGLUS_AK_INVALID_ARGUMENT,
    };
    let Some(host) = handle.inner.as_mut() else {
        return SIGLUS_AK_INVALID_STATE;
    };
    let desc = host.renderer_mut().offscreen_frame_desc();
    let Some((width, height, stride)) = desc else {
        handle.last_error = "renderer is not offscreen".to_string();
        return SIGLUS_AK_NOT_SUPPORTED;
    };
    if !out_width.is_null() {
        *out_width = width;
    }
    if !out_height.is_null() {
        *out_height = height;
    }
    if !out_stride_bytes.is_null() {
        *out_stride_bytes = stride;
    }
    SIGLUS_AK_OK
}

/// Copies the latest rendered frame as tightly packed RGBA8 into
/// `out_pixels`. Requires `out_size >= width * height * 4`; call after every
/// successful [`siglus_ak_step`].
///
/// # Safety
/// `out_pixels` must point to `out_size` writable bytes.
#[no_mangle]
pub unsafe extern "C" fn siglus_ak_read_frame_rgba(
    handle: *mut SiglusAetherHost,
    out_pixels: *mut u8,
    out_size: usize,
) -> i32 {
    let handle = match handle.as_mut() {
        Some(h) => h,
        None => return SIGLUS_AK_INVALID_ARGUMENT,
    };
    if out_pixels.is_null() {
        handle.last_error = "out_pixels is null".to_string();
        return SIGLUS_AK_INVALID_ARGUMENT;
    }
    let Some(host) = handle.inner.as_mut() else {
        handle.last_error = "no game is open".to_string();
        return SIGLUS_AK_INVALID_STATE;
    };
    // Scope the renderer borrow so error reporting on `handle` afterwards is
    // conflict-free.
    let read_result = {
        let mut renderer = host.renderer_mut();
        match renderer.offscreen_frame_desc() {
            None => Err(anyhow::anyhow!("renderer is not offscreen")),
            Some((width, height, _stride)) => {
                let needed = width as usize * height as usize * 4;
                if out_size < needed {
                    Err(anyhow::anyhow!(
                        "frame buffer too small: need {needed} bytes, got {out_size}"
                    ))
                } else {
                    let out = std::slice::from_raw_parts_mut(out_pixels, needed);
                    renderer.read_offscreen_rgba(out)
                }
            }
        }
    };
    match read_result {
        Ok(()) => SIGLUS_AK_OK,
        Err(e) => handle.record_error(e),
    }
}

/// Borrowed MTLDevice for allocating same-device IOSurface textures. Null on
/// non-Metal backends. The pointer is valid only while this renderer is alive.
#[no_mangle]
pub unsafe extern "C" fn siglus_ak_metal_device(handle: *mut SiglusAetherHost) -> *mut c_void {
    #[cfg(target_vendor = "apple")]
    if let Some(host) = handle.as_mut().and_then(|h| h.inner.as_mut()) {
        return crate::render::shared_metal::SharedMetalPresenter::device_ptr(&host.renderer_mut().device);
    }
    let _ = handle;
    std::ptr::null_mut()
}

/// Select a retained, same-device BGRA8 render target. A null target pauses
/// publication while the host waits for a retired buffer; it does not pause VM.
#[no_mangle]
pub unsafe extern "C" fn siglus_ak_select_metal_target(
    handle: *mut SiglusAetherHost, texture: *mut c_void, width: u32, height: u32,
) -> i32 {
    let Some(handle) = handle.as_mut() else { return SIGLUS_AK_INVALID_ARGUMENT; };
    #[cfg(target_vendor = "apple")]
    {
        let Some(host) = handle.inner.as_mut() else { return SIGLUS_AK_INVALID_STATE; };
        let result = {
            let mut renderer = host.renderer_mut();
            let renderer = &mut *renderer;
            if !texture.is_null() && renderer.offscreen_size() != (width, height) {
                return SIGLUS_AK_INVALID_ARGUMENT;
            }
            renderer.shared_presentation.select(&renderer.device, texture, width, height)
        };
        return match result { Ok(()) => SIGLUS_AK_OK, Err(err) => handle.record_error(err) };
    }
    #[cfg(not(target_vendor = "apple"))]
    { let _ = (handle, texture, width, height); SIGLUS_AK_NOT_SUPPORTED }
}

/// Reports whether the currently selected shared target has finished the
/// producer submission. This is a non-blocking poll used by the C++ target
/// ring; false keeps the previous published surface visible.
#[no_mangle]
pub unsafe extern "C" fn siglus_ak_metal_target_ready(
    handle: *mut SiglusAetherHost,
    texture: *mut c_void,
) -> i32 {
    let Some(handle) = handle.as_mut() else { return 0; };
    #[cfg(target_vendor = "apple")]
    if let Some(host) = handle.inner.as_mut() {
        return host.renderer_mut().shared_presentation.target_ready(texture) as i32;
    }
    let _ = (handle, texture);
    0
}

#[no_mangle]
pub unsafe extern "C" fn siglus_ak_clear_metal_targets(handle: *mut SiglusAetherHost) {
    #[cfg(target_vendor = "apple")]
    if let Some(host) = handle.as_mut().and_then(|h| h.inner.as_mut()) {
        let mut renderer = host.renderer_mut();
        let renderer = &mut *renderer;
        renderer.shared_presentation.clear(&renderer.device);
    }
    let _ = handle;
}

fn map_button(button: i32) -> VmMouseButton {
    match button {
        0 => VmMouseButton::Left,
        1 => VmMouseButton::Right,
        2 => VmMouseButton::Middle,
        other => VmMouseButton::Other(other.clamp(0, u8::MAX as i32) as u8),
    }
}

/// Cancels a reclassified pointer gesture without generating a click/release.
#[no_mangle]
pub unsafe extern "C" fn siglus_ak_pointer_cancel(handle: *mut SiglusAetherHost) -> i32 {
    let Some(handle) = handle.as_mut() else { return SIGLUS_AK_INVALID_ARGUMENT; };
    handle.with_inner(|host| host.cancel_pointer())
}

#[no_mangle]
pub unsafe extern "C" fn siglus_ak_set_paused(handle: *mut SiglusAetherHost, paused: i32) -> i32 {
    let Some(handle) = handle.as_mut() else { return SIGLUS_AK_INVALID_ARGUMENT; };
    handle.with_inner(|host| host.set_host_paused(paused != 0))
}

#[no_mangle]
pub unsafe extern "C" fn siglus_ak_cache_stats(handle: *mut SiglusAetherHost, cpu_bytes: *mut u64,
    gpu_bytes: *mut u64, cpu_limit: *mut u64, gpu_limit: *mut u64) -> i32 {
    let Some(handle) = handle.as_mut() else { return SIGLUS_AK_INVALID_ARGUMENT; };
    handle.with_inner(|host| {
        if !cpu_bytes.is_null() { *cpu_bytes = host.vm_mut().ctx.images.resident_bytes() as u64; }
        if !cpu_limit.is_null() { *cpu_limit = host.vm_mut().ctx.images.cache_budget_bytes as u64; }
        let renderer = host.renderer_mut();
        if !gpu_bytes.is_null() { *gpu_bytes = renderer.texture_cache_bytes(); }
        if !gpu_limit.is_null() { *gpu_limit = renderer.texture_cache_budget_bytes; }
    })
}

#[no_mangle]
pub unsafe extern "C" fn siglus_ak_debug_info(handle: *mut SiglusAetherHost, output: *mut c_char, size: usize) -> i32 {
    let Some(handle) = handle.as_mut() else { return SIGLUS_AK_INVALID_ARGUMENT; };
    if output.is_null() || size == 0 { return SIGLUS_AK_INVALID_ARGUMENT; }
    let mut fits = true;
    let result = handle.with_inner(|host| {
        let text = host.debug_status_summary();
        fits = text.len() < size;
        let n = text.len().min(size - 1);
        std::ptr::copy_nonoverlapping(text.as_ptr(), output.cast(), n);
        *output.add(n) = 0;
    });
    if fits { result } else { SIGLUS_AK_INVALID_ARGUMENT }
}

/// # Safety
/// `text_utf8` must be a valid NUL-terminated UTF-8 string when non-null.
#[no_mangle]
pub unsafe extern "C" fn siglus_ak_mouse_move(
    handle: *mut SiglusAetherHost,
    x: f64,
    y: f64,
) -> i32 {
    let handle = match handle.as_mut() {
        Some(h) => h,
        None => return SIGLUS_AK_INVALID_ARGUMENT,
    };
    handle.with_inner(|host| {
        host.mouse_move(x, y);
    })
}

/// Button: 0 = left, 1 = right, 2 = middle.
///
/// # Safety
/// `handle` must be a live handle returned by [`siglus_ak_create`].
#[no_mangle]
pub unsafe extern "C" fn siglus_ak_mouse_button(
    handle: *mut SiglusAetherHost,
    button: i32,
    pressed: i32,
) -> i32 {
    let handle = match handle.as_mut() {
        Some(h) => h,
        None => return SIGLUS_AK_INVALID_ARGUMENT,
    };
    let mapped = map_button(button);
    handle.with_inner(|host| {
        if pressed != 0 {
            host.mouse_down(mapped);
        } else {
            host.mouse_up(mapped);
        }
    })
}

/// Touch phase uses the same convention as the upstream mobile hosts:
/// 0 = begin, 1 = move, 2 = end.
///
/// # Safety
/// `handle` must be a live handle returned by [`siglus_ak_create`].
#[no_mangle]
pub unsafe extern "C" fn siglus_ak_touch(
    handle: *mut SiglusAetherHost,
    phase: i32,
    x: f64,
    y: f64,
) -> i32 {
    let handle = match handle.as_mut() {
        Some(h) => h,
        None => return SIGLUS_AK_INVALID_ARGUMENT,
    };
    handle.with_inner(|host| {
        host.touch(phase, x, y);
    })
}

#[no_mangle]
pub unsafe extern "C" fn siglus_ak_mouse_wheel(
    handle: *mut SiglusAetherHost,
    delta_y: i32,
) -> i32 {
    let handle = match handle.as_mut() {
        Some(h) => h,
        None => return SIGLUS_AK_INVALID_ARGUMENT,
    };
    handle.with_inner(|host| {
        host.mouse_wheel(delta_y);
    })
}

/// Platform key code follows the upstream convention (`key_down_code`):
/// VK-style codes (0x1B escape, 0x0D enter, ASCII for letters/digits, ...).
///
/// # Safety
/// `handle` must be a live handle returned by [`siglus_ak_create`].
#[no_mangle]
pub unsafe extern "C" fn siglus_ak_key(
    handle: *mut SiglusAetherHost,
    key_code: i32,
    pressed: i32,
) -> i32 {
    let handle = match handle.as_mut() {
        Some(h) => h,
        None => return SIGLUS_AK_INVALID_ARGUMENT,
    };
    handle.with_inner(|host| {
        if pressed != 0 {
            host.key_down_code(key_code);
        } else {
            host.key_up_code(key_code);
        }
    })
}

/// # Safety
/// `text_utf8` must be a valid NUL-terminated UTF-8 string when non-null.
#[no_mangle]
pub unsafe extern "C" fn siglus_ak_text_input(
    handle: *mut SiglusAetherHost,
    text_utf8: *const c_char,
) -> i32 {
    let handle = match handle.as_mut() {
        Some(h) => h,
        None => return SIGLUS_AK_INVALID_ARGUMENT,
    };
    let text = if text_utf8.is_null() {
        String::new()
    } else {
        CStr::from_ptr(text_utf8).to_string_lossy().into_owned()
    };
    handle.with_inner(|host| {
        host.text_input(&text);
    })
}

/// Installs the native message box callback used instead of the engine's own
/// UI. Pass null to fall back to engine-internal handling.
///
/// # Safety
/// `callback` must be a valid function pointer when non-null; `user_data` is
/// forwarded opaquely.
#[no_mangle]
pub unsafe extern "C" fn siglus_ak_set_messagebox_callback(
    handle: *mut SiglusAetherHost,
    callback: Option<SiglusNativeMessageBoxCallback>,
    user_data: *mut c_void,
) -> i32 {
    let handle = match handle.as_mut() {
        Some(h) => h,
        None => return SIGLUS_AK_INVALID_ARGUMENT,
    };
    handle.with_inner(|host| {
        host.set_native_messagebox_callback(callback, user_data);
    })
}

/// Delivers the user's answer for a message box previously announced through
/// the callback registered with [`siglus_ak_set_messagebox_callback`].
///
/// # Safety
/// `handle` must be a live handle returned by [`siglus_ak_create`].
#[no_mangle]
pub unsafe extern "C" fn siglus_ak_submit_messagebox_result(
    handle: *mut SiglusAetherHost,
    request_id: u64,
    value: i64,
) -> i32 {
    let handle = match handle.as_mut() {
        Some(h) => h,
        None => return SIGLUS_AK_INVALID_ARGUMENT,
    };
    handle.with_inner(|host| {
        host.submit_native_messagebox_result(request_id, value);
    })
}

#[no_mangle]
pub unsafe extern "C" fn siglus_ak_set_platform_request_callback(
    handle: *mut SiglusAetherHost,
    callback: Option<crate::runtime::platform::RequestCallback>,
    user_data: *mut c_void,
) -> i32 {
    let Some(handle) = handle.as_mut() else { return SIGLUS_AK_INVALID_ARGUMENT; };
    handle.with_inner(|host| host.set_platform_request_callback(callback, user_data))
}

#[no_mangle]
pub unsafe extern "C" fn siglus_ak_submit_platform_response(
    handle: *mut SiglusAetherHost, operation: *const c_char, argument: *const c_char,
) -> i32 {
    let Some(handle) = handle.as_mut() else { return SIGLUS_AK_INVALID_ARGUMENT; };
    if operation.is_null() || argument.is_null() { return SIGLUS_AK_INVALID_ARGUMENT; }
    let (Ok(operation), Ok(argument)) = (CStr::from_ptr(operation).to_str(), CStr::from_ptr(argument).to_str()) else { return SIGLUS_AK_INVALID_ARGUMENT; };
    let Some(host) = handle.inner.as_mut() else { return SIGLUS_AK_INVALID_STATE; };
    match host.submit_platform_response(operation, argument) {
        Ok(true) => SIGLUS_AK_OK,
        Ok(false) => SIGLUS_AK_NOT_SUPPORTED,
        Err(error) => handle.record_error(error),
    }
}

/// Returns the last error message for this handle. The pointer stays valid
/// until the next FFI call on the same handle or its destruction.
///
/// # Safety
/// `handle` may be null (returns a static placeholder).
#[no_mangle]
pub unsafe extern "C" fn siglus_ak_last_error(handle: *mut SiglusAetherHost) -> *const c_char {
    match handle.as_mut() {
        Some(h) => {
            h.last_error_cstr = CString::new(h.last_error.replace('\0', "\\0"))
                .expect("interior NUL bytes were escaped");
            h.last_error_cstr.as_ptr()
        }
        // Only reached with a null handle; fine to hand back a static string.
        None => b"\0".as_ptr() as *const c_char,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn last_error_is_a_valid_c_string_before_and_after_failure() {
        unsafe {
            let handle = siglus_ak_create(1.0);
            assert_eq!(CStr::from_ptr(siglus_ak_last_error(handle)).to_bytes(), b"");
            assert_eq!(siglus_ak_step(handle, 16), SIGLUS_AK_INVALID_STATE);
            assert_eq!(
                CStr::from_ptr(siglus_ak_last_error(handle)).to_bytes(),
                b"no game is open"
            );
            siglus_ak_close(handle);
            assert_eq!(CStr::from_ptr(siglus_ak_last_error(handle)).to_bytes(), b"");
            siglus_ak_destroy(handle);
            assert_eq!(
                CStr::from_ptr(siglus_ak_last_error(std::ptr::null_mut())).to_bytes(),
                b""
            );
        }
    }

    #[test]
    fn last_error_preserves_unicode_and_escapes_interior_nul() {
        let mut host = SiglusAetherHost::new(1.0);
        host.last_error = "错误\0詳細".to_string();
        let message = unsafe { CStr::from_ptr(siglus_ak_last_error(&mut host)) };
        assert_eq!(message.to_str().unwrap(), "错误\\0詳細");
    }
}
