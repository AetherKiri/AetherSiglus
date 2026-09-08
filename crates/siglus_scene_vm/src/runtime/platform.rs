//! Optional host services. Requests are asynchronous and never execute inside
//! the VM; an embedding host owns dialogs, window state and external opening.

use std::collections::HashMap;
use std::ffi::{c_char, c_void, CString};

pub type RequestCallback = unsafe extern "C" fn(*mut c_void, *const c_char, *const c_char);

#[derive(Default)]
pub struct PlatformBridge {
    callback: Option<RequestCallback>,
    user_data: usize,
    next_id: u64,
}

impl PlatformBridge {
    pub fn set_callback(&mut self, callback: Option<RequestCallback>, user_data: *mut c_void) {
        self.callback = callback;
        self.user_data = user_data as usize;
    }

    pub fn available(&self) -> bool {
        self.callback.is_some()
    }

    pub fn next_id(&mut self) -> u64 {
        self.next_id = self.next_id.wrapping_add(1).max(1);
        self.next_id
    }

    pub fn request(&self, operation: &str, fields: &[(&str, &str)]) -> bool {
        let Some(callback) = self.callback else {
            return false;
        };
        let Ok(operation) = CString::new(operation) else {
            return false;
        };
        let argument = fields
            .iter()
            .map(|(k, v)| format!("{}={}", encode(k), encode(v)))
            .collect::<Vec<_>>()
            .join("&");
        let argument = CString::new(argument).expect("encoded fields contain no NUL");
        unsafe {
            callback(
                self.user_data as *mut c_void,
                operation.as_ptr(),
                argument.as_ptr(),
            );
        }
        true
    }
}

pub fn encode(value: &str) -> String {
    let mut out = String::new();
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            out.push(byte as char);
        } else {
            use std::fmt::Write;
            let _ = write!(out, "%{byte:02X}");
        }
    }
    out
}

fn decode(value: &str) -> Option<String> {
    let mut out = Vec::new();
    let bytes = value.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            if i + 2 >= bytes.len() {
                return None;
            }
            let h = (bytes[i + 1] as char).to_digit(16)?;
            let l = (bytes[i + 2] as char).to_digit(16)?;
            out.push((h * 16 + l) as u8);
            i += 3;
        } else {
            out.push(if bytes[i] == b'+' { b' ' } else { bytes[i] });
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

pub fn parse_form(value: &str) -> Option<HashMap<String, String>> {
    value
        .split('&')
        .filter(|v| !v.is_empty())
        .map(|field| {
            let (key, value) = field.split_once('=').unwrap_or((field, ""));
            Some((decode(key)?, decode(value)?))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn form_round_trip_preserves_unicode_paths_and_delimiters() {
        let path = "/savedata/截图 & +.bmp";
        assert_eq!(
            parse_form(&format!("path={}", encode(path))).unwrap()["path"],
            path
        );
        assert!(parse_form("path=%FF").is_none());
        assert!(parse_form("path=%A").is_none());
    }
}
