//! Opt-in slow-operation timings; no clock reads or allocations when disabled.
use crate::platform_time::Instant;
use std::sync::OnceLock;

pub(crate) fn enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| std::env::var_os("SIGLUS_PERF_TRACE").is_some())
}

pub(crate) struct Span {
    name: &'static str,
    start: Option<Instant>,
    detail: Option<String>,
}

impl Span {
    pub(crate) fn new(name: &'static str) -> Self {
        Self { name, start: enabled().then(Instant::now), detail: None }
    }

    pub(crate) fn with_detail(name: &'static str, detail: impl FnOnce() -> String) -> Self {
        let detail = enabled().then(detail);
        Self { name, start: enabled().then(Instant::now), detail }
    }
}

impl Drop for Span {
    fn drop(&mut self) {
        let Some(start) = self.start else { return; };
        let ms = start.elapsed().as_secs_f64() * 1000.0;
        if ms >= 10.0 {
            eprintln!("[SG_PERF] op={} ms={ms:.3} {}", self.name, self.detail.as_deref().unwrap_or(""));
        }
    }
}
