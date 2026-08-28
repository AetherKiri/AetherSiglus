//! Process-lifetime cache for the debug/trace environment flags.
//!
//! The VM used to call `std::env::var*` for every opcode dispatch, object op,
//! and frame; `getenv` dominated the profile while tracing was off. Every flag
//! is a process-lifetime constant, so consult the environment once per key on
//! first use and serve later reads from a static table.
//!
//! Semantics preserved per call site:
//! - [`is_set`]: `std::env::var_os(key).is_some()`
//! - [`is_truthy`]: value is `"1" | "true" | "TRUE" | "yes" | "YES"`
//! - [`value`]: the raw UTF-8 value, if present

use std::sync::OnceLock;

#[derive(Clone, Default)]
struct FlagValue {
    present: bool,
    truthy: bool,
    raw: Option<String>,
}

impl FlagValue {
    fn from_env(key: &str) -> Self {
        let raw = std::env::var_os(key).and_then(|v| v.into_string().ok());
        let truthy = raw
            .as_deref()
            .map(|v| matches!(v, "1" | "true" | "TRUE" | "yes" | "YES"))
            .unwrap_or(false);
        Self {
            present: raw.is_some(),
            truthy,
            raw,
        }
    }
}

/// Stable slot index per cached key. A `match` keeps lookups branch-predicted
/// and hashing-free; the hot call sites run multiple times per VM opcode.
fn slot_index(key: &str) -> Option<usize> {
    Some(match key {
        "SG_DEBUG" => 0,
        "SG_PROC_FLOW_TRACE" => 1,
        "SG_CTX_TICK_TRACE" => 2,
        "SG_MSGBK_TRACE" => 3,
        "SG_MOVIE_TRACE" => 4,
        "SG_SAVELOAD_TRACE" => 5,
        "SG_OBJECT_MOTION_TRACE" => 6,
        "SG_INPUT_TRACE" => 7,
        "SG_MWND_OBJECT_TRACE" => 8,
        "SG_RENDER_TREE_DEBUG" => 9,
        "SG_CONFIG_BUTTON_TRACE" => 10,
        "SG_AUDIO_TRACE" => 11,
        "SG_TICK_TRACE" => 12,
        "SG_FRAME_ACTION_TRACE" => 13,
        "SG_SYSCOM_PROC_TRACE" => 14,
        "SG_COUNTER_TRACE" => 15,
        "SG_TITLE_CHAIN_TRACE" => 16,
        "SG_TRACE_OBJECT_SLOT" => 17,
        "SIGLUS_TRACE_VM" => 18,
        "SIGLUS_TRACE_VM_SCENE" => 19,
        "SIGLUS_TRACE_VM_PC" => 20,
        "SIGLUS_TRACE_VM_COMMANDS" => 21,
        "SIGLUS_TRACE_CALL_RETURN_PC" => 22,
        "SIGLUS_TRACE_FRAME_ACTION_CALL" => 23,
        "SIGLUS_TRACE_CODES" => 24,
        "SIGLUS_TRACE_UNKNOWN_FORMS" => 25,
        "SIGLUS_INLINE_USER_CMD_MAX_STEPS" => 26,
        "SIGLUS_FRAME_ACTION_MAX_STEPS" => 27,
        _ => return None,
    })
}

const SLOT_COUNT: usize = 28;

fn table() -> &'static [FlagValue; SLOT_COUNT] {
    static TABLE: OnceLock<[FlagValue; SLOT_COUNT]> = OnceLock::new();
    TABLE.get_or_init(|| {
        let mut slots: [FlagValue; SLOT_COUNT] = Default::default();
        for idx in 0..SLOT_COUNT {
            let key = slot_key(idx).expect("slot key must exist");
            slots[idx] = FlagValue::from_env(key);
        }
        slots
    })
}

/// Inverse of [`slot_index`] so the table can be filled without a HashMap.
fn slot_key(idx: usize) -> Option<&'static str> {
    Some(match idx {
        0 => "SG_DEBUG",
        1 => "SG_PROC_FLOW_TRACE",
        2 => "SG_CTX_TICK_TRACE",
        3 => "SG_MSGBK_TRACE",
        4 => "SG_MOVIE_TRACE",
        5 => "SG_SAVELOAD_TRACE",
        6 => "SG_OBJECT_MOTION_TRACE",
        7 => "SG_INPUT_TRACE",
        8 => "SG_MWND_OBJECT_TRACE",
        9 => "SG_RENDER_TREE_DEBUG",
        10 => "SG_CONFIG_BUTTON_TRACE",
        11 => "SG_AUDIO_TRACE",
        12 => "SG_TICK_TRACE",
        13 => "SG_FRAME_ACTION_TRACE",
        14 => "SG_SYSCOM_PROC_TRACE",
        15 => "SG_COUNTER_TRACE",
        16 => "SG_TITLE_CHAIN_TRACE",
        17 => "SG_TRACE_OBJECT_SLOT",
        18 => "SIGLUS_TRACE_VM",
        19 => "SIGLUS_TRACE_VM_SCENE",
        20 => "SIGLUS_TRACE_VM_PC",
        21 => "SIGLUS_TRACE_VM_COMMANDS",
        22 => "SIGLUS_TRACE_CALL_RETURN_PC",
        23 => "SIGLUS_TRACE_FRAME_ACTION_CALL",
        24 => "SIGLUS_TRACE_CODES",
        25 => "SIGLUS_TRACE_UNKNOWN_FORMS",
        26 => "SIGLUS_INLINE_USER_CMD_MAX_STEPS",
        27 => "SIGLUS_FRAME_ACTION_MAX_STEPS",
        _ => return None,
    })
}

/// Whether `key` is present in the environment (any value, including empty).
pub fn is_set(key: &str) -> bool {
    match slot_index(key) {
        Some(idx) => table()[idx].present,
        None => std::env::var_os(key).is_some(),
    }
}

/// Whether `key` holds one of the truthy spellings used by the trace helpers.
pub fn is_truthy(key: &str) -> bool {
    match slot_index(key) {
        Some(idx) => table()[idx].truthy,
        None => FlagValue::from_env(key).truthy,
    }
}

/// The raw UTF-8 value of `key`, if present.
pub fn value(key: &str) -> Option<&'static str> {
    match slot_index(key) {
        Some(idx) => table()[idx].raw.as_deref(),
        None => None,
    }
}
