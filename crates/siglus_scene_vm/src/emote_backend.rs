//! Host-supplied E-mote ABI. Private SDK code is never linked into this crate.
use anyhow::{anyhow, bail, ensure, Result};
use std::ffi::{c_char, c_void, CString};
use std::marker::PhantomData;
use std::rc::Rc;
use std::sync::{Arc, OnceLock};

#[repr(C)]
pub struct Source {
    pub bytes: *const u8,
    pub size: usize,
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct Backend {
    pub version: u32,
    pub size: u32,
    pub create:
        Option<unsafe extern "C" fn(*const Source, usize, *mut c_char, usize) -> *mut c_void>,
    pub destroy: Option<unsafe extern "C" fn(*mut c_void)>,
    pub clone_player: Option<unsafe extern "C" fn(*mut c_void, *mut c_char, usize) -> *mut c_void>,
    pub control: Option<
        unsafe extern "C" fn(
            *mut c_void,
            u32,
            *const c_char,
            f64,
            u32,
            *mut i32,
            *mut c_char,
            usize,
        ) -> i32,
    >,
    pub render_rgba: Option<
        unsafe extern "C" fn(
            *mut c_void,
            u32,
            u32,
            f32,
            f32,
            *mut u8,
            usize,
            *mut c_char,
            usize,
        ) -> i32,
    >,
}

static BACKEND: OnceLock<Backend> = OnceLock::new();

#[no_mangle]
pub unsafe extern "C" fn siglus_ak_register_emote_backend(backend: *const Backend) -> i32 {
    let Some(backend) = backend.as_ref() else {
        return -1;
    };
    if backend.version != 1
        || backend.size as usize != std::mem::size_of::<Backend>()
        || backend.create.is_none()
        || backend.destroy.is_none()
        || backend.clone_player.is_none()
        || backend.control.is_none()
        || backend.render_rgba.is_none()
    {
        return -1;
    }
    if BACKEND.set(*backend).is_err() {
        return -2;
    }
    0
}

#[derive(Debug)]
struct Instance {
    api: Backend,
    handle: usize,
}
impl Drop for Instance {
    fn drop(&mut self) {
        unsafe { (self.api.destroy.unwrap())(self.handle as *mut c_void) }
    }
}

#[derive(Debug, Clone)]
pub struct NativePlayer {
    inner: Arc<Instance>,
    // The SDK owns thread-affine rendering state. Do not let safe Rust move
    // a player to a worker or query/render it concurrently through &self.
    engine_thread: PhantomData<Rc<()>>,
}

fn error_text(error: &[c_char]) -> String {
    let bytes: Vec<u8> = error
        .iter()
        .take_while(|c| **c != 0)
        .map(|c| *c as u8)
        .collect();
    let text = String::from_utf8_lossy(&bytes);
    if text.is_empty() {
        "host E-mote backend failed".into()
    } else {
        text.into_owned()
    }
}

impl NativePlayer {
    pub fn create(sources: &[Vec<u8>]) -> Result<Option<Self>> {
        let Some(api) = BACKEND.get().copied() else {
            return Ok(None);
        };
        Self::create_with(api, sources).map(Some)
    }

    fn create_with(api: Backend, sources: &[Vec<u8>]) -> Result<Self> {
        ensure!(
            !sources.is_empty() && sources.len() <= 64,
            "invalid E-mote source count"
        );
        let sources: Vec<_> = sources
            .iter()
            .map(|s| Source {
                bytes: s.as_ptr(),
                size: s.len(),
            })
            .collect();
        let mut error = [0 as c_char; 1024];
        let handle = unsafe {
            (api.create.unwrap())(
                sources.as_ptr(),
                sources.len(),
                error.as_mut_ptr(),
                error.len(),
            )
        };
        ensure!(!handle.is_null(), "{}", error_text(&error));
        Ok(Self {
            inner: Arc::new(Instance {
                api,
                handle: handle as usize,
            }),
            engine_thread: PhantomData,
        })
    }

    pub fn fork(&self) -> Result<Self> {
        let mut error = [0 as c_char; 1024];
        let handle = unsafe {
            (self.inner.api.clone_player.unwrap())(
                self.inner.handle as *mut c_void,
                error.as_mut_ptr(),
                error.len(),
            )
        };
        ensure!(!handle.is_null(), "{}", error_text(&error));
        Ok(Self {
            inner: Arc::new(Instance {
                api: self.inner.api,
                handle: handle as usize,
            }),
            engine_thread: PhantomData,
        })
    }

    pub fn control(&mut self, op: u32, label: &str, value: f64, flags: u32) -> Result<i32> {
        // ObjectState clones held by savepoints must not share mutable SDK
        // state. Render packets contain pixels, so this does not clone on each
        // frame merely because the renderer retains its previous packet.
        if Arc::strong_count(&self.inner) > 1 {
            *self = self.fork()?;
        }
        self.query(op, label, value, flags)
    }

    pub fn is_animating(&self) -> Result<bool> {
        self.query(8, "", 0.0, 0).map(|value| value != 0)
    }

    fn query(&self, op: u32, label: &str, value: f64, flags: u32) -> Result<i32> {
        let label = CString::new(label)?;
        let mut error = [0 as c_char; 1024];
        let mut result = 0;
        let status = unsafe {
            (self.inner.api.control.unwrap())(
                self.inner.handle as *mut c_void,
                op,
                label.as_ptr(),
                value,
                flags,
                &mut result,
                error.as_mut_ptr(),
                error.len(),
            )
        };
        ensure!(status == 0, "{}", error_text(&error));
        Ok(result)
    }

    pub fn render(&self, width: u32, height: u32, rep_x: f32, rep_y: f32) -> Result<Arc<Vec<u8>>> {
        let size = (width as usize)
            .checked_mul(height as usize)
            .and_then(|n| n.checked_mul(4))
            .ok_or_else(|| anyhow!("E-mote target size overflow"))?;
        if size == 0 || size > 256 * 1024 * 1024 {
            bail!("E-mote target exceeds 256 MiB");
        }
        let mut pixels = vec![0; size];
        let mut error = [0 as c_char; 1024];
        let status = unsafe {
            (self.inner.api.render_rgba.unwrap())(
                self.inner.handle as *mut c_void,
                width,
                height,
                rep_x,
                rep_y,
                pixels.as_mut_ptr(),
                pixels.len(),
                error.as_mut_ptr(),
                error.len(),
            )
        };
        ensure!(status == 0, "{}", error_text(&error));
        Ok(Arc::new(pixels))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    static CREATED: AtomicUsize = AtomicUsize::new(0);
    static DESTROYED: AtomicUsize = AtomicUsize::new(0);
    unsafe extern "C" fn create(
        sources: *const Source,
        count: usize,
        _: *mut c_char,
        _: usize,
    ) -> *mut c_void {
        assert_eq!(count, 2);
        assert_eq!((*sources).size, 3);
        assert_eq!((*sources.add(1)).size, 4);
        CREATED.fetch_add(1, Ordering::Relaxed);
        Box::into_raw(Box::new(0i32)).cast()
    }
    unsafe extern "C" fn destroy(handle: *mut c_void) {
        drop(Box::from_raw(handle.cast::<i32>()));
        DESTROYED.fetch_add(1, Ordering::Relaxed);
    }
    unsafe extern "C" fn clone(handle: *mut c_void, _: *mut c_char, _: usize) -> *mut c_void {
        CREATED.fetch_add(1, Ordering::Relaxed);
        Box::into_raw(Box::new(*handle.cast::<i32>())).cast()
    }
    unsafe extern "C" fn control(
        handle: *mut c_void,
        op: u32,
        _: *const c_char,
        value: f64,
        _: u32,
        out: *mut i32,
        _: *mut c_char,
        _: usize,
    ) -> i32 {
        if op == 1 {
            *handle.cast::<i32>() += value as i32;
        }
        *out = *handle.cast::<i32>();
        0
    }
    unsafe extern "C" fn render(
        _: *mut c_void,
        _: u32,
        _: u32,
        _: f32,
        _: f32,
        data: *mut u8,
        size: usize,
        _: *mut c_char,
        _: usize,
    ) -> i32 {
        std::slice::from_raw_parts_mut(data, size)
            .chunks_exact_mut(4)
            .for_each(|p| p.copy_from_slice(&[10, 20, 30, 40]));
        0
    }
    #[test]
    fn multi_sources_use_one_player_and_clones_own_independent_state() {
        let api = Backend {
            version: 1,
            size: std::mem::size_of::<Backend>() as u32,
            create: Some(create),
            destroy: Some(destroy),
            clone_player: Some(clone),
            control: Some(control),
            render_rgba: Some(render),
        };
        {
            let mut player = NativePlayer::create_with(api, &[vec![1; 3], vec![2; 4]]).unwrap();
            let saved = player.clone();
            player.control(1, "", 16.0, 0).unwrap();
            assert_eq!(player.query(8, "", 0.0, 0).unwrap(), 16);
            assert_eq!(saved.query(8, "", 0.0, 0).unwrap(), 0);
            assert_eq!(
                player.render(1, 1, 0.0, 0.0).unwrap().as_slice(),
                [10, 20, 30, 40]
            );
            assert!(player.render(u32::MAX, 2, 0.0, 0.0).is_err());
        }
        assert_eq!(CREATED.load(Ordering::Relaxed), 2);
        assert_eq!(DESTROYED.load(Ordering::Relaxed), 2);
    }
}
