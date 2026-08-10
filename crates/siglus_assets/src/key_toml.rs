use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};

use crate::angou::{self, AngouStepKind};

pub fn load_key16_from_project_dir(project_dir: &Path) -> Result<Option<[u8; 16]>> {
    let path = project_dir.join("key.toml");
    if !path.is_file() {
        return Ok(None);
    }
    load_key16_from_file(&path)
}

pub fn load_emote_key_from_project_dir(project_dir: &Path) -> Result<Option<u32>> {
    let path = project_dir.join("key.toml");
    if !path.is_file() {
        return Ok(None);
    }
    load_emote_key_from_file(&path)
}

pub fn load_emote_key_from_file(path: &Path) -> Result<Option<u32>> {
    let text = fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    parse_emote_key_toml(&text)
}

pub fn parse_emote_key_toml(text: &str) -> Result<Option<u32>> {
    parse_named_u32(text, "emote_key")
}

pub fn load_key16_from_file(path: &Path) -> Result<Option<[u8; 16]>> {
    let text = fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    parse_key16_toml(&text)
}

pub fn parse_key16_toml(text: &str) -> Result<Option<[u8; 16]>> {
    let Some(bytes) = parse_named_bytes(text, "key", "key_hex")? else {
        return Ok(None);
    };
    if bytes.len() != 16 {
        bail!(
            "key.toml: key must contain exactly 16 bytes, got {}",
            bytes.len()
        );
    }
    let mut out = [0u8; 16];
    out.copy_from_slice(&bytes);
    Ok(Some(out))
}

#[derive(Debug, Clone, Default)]
pub struct KeyTomlConfig {
    pub exe_key16: Option<[u8; 16]>,
    pub base_angou_code: Option<Vec<u8>>,
    pub game_angou_code: Option<Vec<u8>>,
    pub chain_order: Option<Vec<AngouStepKind>>,
    /// Single variable DWORD from the canonical Emote PSB key state
    /// `0x075BCD15, 0x159A55E5, 0x1F123BB5, emote_key, 0, 0`.
    pub emote_key: Option<u32>,
}

pub fn load_key_toml_from_project_dir(project_dir: &Path) -> Result<Option<KeyTomlConfig>> {
    let path = project_dir.join("key.toml");
    if !path.is_file() {
        return Ok(None);
    }
    load_key_toml_from_file(&path).map(Some)
}

pub fn load_key_toml_from_file(path: &Path) -> Result<KeyTomlConfig> {
    let text = fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    parse_key_toml(&text)
}

pub fn parse_key_toml(text: &str) -> Result<KeyTomlConfig> {
    let mut out = KeyTomlConfig::default();

    out.exe_key16 = parse_key16_toml(text)?;
    out.base_angou_code = parse_named_bytes(text, "base_angou_code", "base_angou_hex")?;
    out.game_angou_code = parse_named_bytes(text, "game_angou_code", "game_angou_hex")?;
    out.chain_order = parse_chain_order(text)?;
    out.emote_key = parse_named_u32(text, "emote_key")?;

    Ok(out)
}

/// Atomically add or replace `emote_key` in `<project>/key.toml` while
/// preserving every unrelated line and comment in the existing file.
///
/// The DWORD is written in the conventional fixed-width hexadecimal form when
/// the key is new. If an existing `emote_key` uses decimal notation, its radix
/// is retained.
pub fn write_emote_key_to_project_dir(project_dir: &Path, key: u32) -> Result<PathBuf> {
    let path = project_dir.join("key.toml");
    write_emote_key_to_file(&path, key)?;
    Ok(path)
}

pub fn write_emote_key_to_file(path: &Path, key: u32) -> Result<()> {
    let old = match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(err) => return Err(err).with_context(|| format!("read {}", path.display())),
    };
    let updated = update_emote_key_toml_text(&old, key);
    atomic_write(&path, updated.as_bytes())?;
    Ok(())
}

/// Text-only helper used by the writer and regression tests.
pub fn update_emote_key_toml_text(text: &str, key: u32) -> String {
    let newline = if text.contains("\r\n") { "\r\n" } else { "\n" };
    let mut out = String::with_capacity(text.len().saturating_add(32));
    let mut replaced = false;

    for line_with_nl in text.split_inclusive('\n') {
        let (body_with_cr, line_nl) = line_with_nl
            .strip_suffix('\n')
            .map(|body| (body, "\n"))
            .unwrap_or((line_with_nl, ""));
        let (body, cr) = body_with_cr
            .strip_suffix('\r')
            .map(|body| (body, "\r"))
            .unwrap_or((body_with_cr, ""));

        if !replaced {
            if let Some(eq) = assignment_eq_for_key(body, "emote_key") {
                let comment = comment_start(body).unwrap_or(body.len());
                if eq < comment {
                    let rhs = &body[eq + 1..comment];
                    let leading_ws_len = rhs.len() - rhs.trim_start().len();
                    let trailing_ws_len = rhs.len() - rhs.trim_end().len();
                    let leading_ws = &rhs[..leading_ws_len];
                    let trailing_ws = if trailing_ws_len == 0 {
                        ""
                    } else {
                        &rhs[rhs.len() - trailing_ws_len..]
                    };
                    let old_value = rhs.trim();
                    let value = format_u32_like(old_value, key);

                    out.push_str(&body[..eq + 1]);
                    out.push_str(leading_ws);
                    out.push_str(&value);
                    out.push_str(trailing_ws);
                    out.push_str(&body[comment..]);
                    out.push_str(cr);
                    out.push_str(line_nl);
                    replaced = true;
                    continue;
                }
            }
        }

        out.push_str(body);
        out.push_str(cr);
        out.push_str(line_nl);
    }

    // `split_inclusive` yields no item for an empty input, and an input without
    // a trailing newline has already been copied above.
    if !replaced {
        let assignment = format!("emote_key = 0x{key:08X}{newline}");
        if contains_toml_table_header(&out) {
            // Appending after a `[table]` header would silently place emote_key
            // inside that table. Keep the new setting at TOML root, after only
            // an optional leading comment/blank-file header.
            let at = root_setting_insertion_offset(&out);
            let mut rooted = String::with_capacity(out.len() + assignment.len() + newline.len());
            rooted.push_str(&out[..at]);
            if at != 0 && !rooted.ends_with('\n') {
                rooted.push_str(newline);
            }
            rooted.push_str(&assignment);
            rooted.push_str(&out[at..]);
            return rooted;
        }
        if !out.is_empty() && !out.ends_with('\n') {
            out.push_str(newline);
        }
        out.push_str(&assignment);
    }

    out
}

fn contains_toml_table_header(text: &str) -> bool {
    text.lines().any(|line| {
        let code = &line[..comment_start(line).unwrap_or(line.len())];
        let code = code.trim();
        code.starts_with('[') && code.ends_with(']') && !code.contains('=')
    })
}

fn root_setting_insertion_offset(text: &str) -> usize {
    let mut offset = 0usize;
    for line in text.split_inclusive('\n') {
        let body = line.trim_end_matches(|ch| ch == '\r' || ch == '\n');
        let trimmed = body.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            offset += line.len();
        } else {
            break;
        }
    }
    offset
}

fn parse_named_bytes(text: &str, key: &str, alt_hex_key: &str) -> Result<Option<Vec<u8>>> {
    if let Some(raw) = collect_rhs_for_key(text, key) {
        return parse_bytes_value(&raw, key);
    }
    if let Some(raw) = collect_rhs_for_key(text, alt_hex_key) {
        return parse_hex_value(&raw, alt_hex_key);
    }
    Ok(None)
}

fn parse_named_u32(text: &str, key: &str) -> Result<Option<u32>> {
    let Some(raw) = collect_scalar_rhs_for_key(text, key) else {
        return Ok(None);
    };
    let raw = raw.trim();
    if raw.is_empty() {
        return Ok(None);
    }
    let raw = raw.trim_matches('"').trim_matches('\'').trim();
    let normalized = raw.replace('_', "");
    let value = if let Some(hex) = normalized
        .strip_prefix("0x")
        .or_else(|| normalized.strip_prefix("0X"))
    {
        u32::from_str_radix(hex, 16)
            .with_context(|| format!("key.toml: invalid hex DWORD for {key}: {raw}"))?
    } else {
        normalized
            .parse::<u32>()
            .with_context(|| format!("key.toml: invalid DWORD for {key}: {raw}"))?
    };
    Ok(Some(value))
}

fn collect_scalar_rhs_for_key<'a>(text: &'a str, key: &str) -> Option<&'a str> {
    for raw_line in text.lines() {
        let comment = comment_start(raw_line).unwrap_or(raw_line.len());
        let code = &raw_line[..comment];
        let Some((lhs, rhs)) = code.split_once('=') else {
            continue;
        };
        if lhs.trim() == key {
            return Some(rhs.trim());
        }
    }
    None
}

fn assignment_eq_for_key(line: &str, key: &str) -> Option<usize> {
    let comment = comment_start(line).unwrap_or(line.len());
    let code = &line[..comment];
    let eq = code.find('=')?;
    (code[..eq].trim() == key).then_some(eq)
}

fn comment_start(line: &str) -> Option<usize> {
    let mut quote: Option<char> = None;
    let mut escaped = false;
    for (idx, ch) in line.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        if ch == '\\' && quote == Some('"') {
            escaped = true;
            continue;
        }
        match ch {
            '"' | '\'' => {
                if quote == Some(ch) {
                    quote = None;
                } else if quote.is_none() {
                    quote = Some(ch);
                }
            }
            '#' if quote.is_none() => return Some(idx),
            _ => {}
        }
    }
    None
}

fn format_u32_like(old_value: &str, key: u32) -> String {
    let old = old_value.trim_matches('"').trim_matches('\'').trim();
    if old.starts_with("0x") || old.starts_with("0X") {
        if old.chars().any(|ch| ch.is_ascii_hexdigit() && ch.is_ascii_lowercase()) {
            format!("0x{key:08x}")
        } else {
            format!("0x{key:08X}")
        }
    } else if old.is_empty() {
        format!("0x{key:08X}")
    } else {
        key.to_string()
    }
}

fn atomic_write(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("path has no parent: {}", path.display()))?;
    let existing_permissions = match fs::metadata(path) {
        Ok(metadata) => {
            let permissions = metadata.permissions();
            if permissions.readonly() {
                bail!("{} is read-only", path.display());
            }
            Some(permissions)
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => None,
        Err(err) => return Err(err).with_context(|| format!("stat {}", path.display())),
    };
    let file_name = path
        .file_name()
        .and_then(|v| v.to_str())
        .unwrap_or("key.toml");
    let pid = std::process::id();

    let mut temp_path = None;
    let mut temp_file = None;
    for attempt in 0..128u32 {
        let candidate = parent.join(format!(".{file_name}.emote-key-{pid}-{attempt}.tmp"));
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&candidate)
        {
            Ok(file) => {
                temp_path = Some(candidate);
                temp_file = Some(file);
                break;
            }
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(err) => {
                return Err(err)
                    .with_context(|| format!("create temporary key.toml beside {}", path.display()))
            }
        }
    }

    let temp_path = temp_path.ok_or_else(|| {
        anyhow::anyhow!("could not allocate temporary file beside {}", path.display())
    })?;
    let mut file = temp_file.expect("temporary file accompanies temporary path");
    let write_result = (|| -> Result<()> {
        file.write_all(bytes)
            .with_context(|| format!("write {}", temp_path.display()))?;
        if let Some(permissions) = existing_permissions {
            file.set_permissions(permissions)
                .with_context(|| format!("preserve permissions on {}", temp_path.display()))?;
        }
        file.sync_all()
            .with_context(|| format!("sync {}", temp_path.display()))?;
        drop(file);
        replace_file_from_temp(&temp_path, path)?;
        sync_parent_directory_best_effort(parent);
        Ok(())
    })();

    if write_result.is_err() {
        let _ = fs::remove_file(&temp_path);
    }
    write_result
}


#[cfg(unix)]
fn sync_parent_directory_best_effort(parent: &Path) {
    if let Ok(dir) = fs::File::open(parent) {
        let _ = dir.sync_all();
    }
}

#[cfg(not(unix))]
fn sync_parent_directory_best_effort(_parent: &Path) {}

#[cfg(not(windows))]
fn replace_file_from_temp(temp_path: &Path, path: &Path) -> Result<()> {
    fs::rename(temp_path, path).with_context(|| {
        format!(
            "replace {} with temporary file {}",
            path.display(),
            temp_path.display()
        )
    })
}

#[cfg(windows)]
fn replace_file_from_temp(temp_path: &Path, path: &Path) -> Result<()> {
    if !path.exists() {
        return fs::rename(temp_path, path).with_context(|| {
            format!(
                "install {} from temporary file {}",
                path.display(),
                temp_path.display()
            )
        });
    }

    // std::fs::rename does not replace an existing destination on Windows.
    // Keep a same-directory backup so a failed install can restore the exact
    // previous key.toml instead of truncating or recreating it.
    let parent = path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("path has no parent: {}", path.display()))?;
    let file_name = path
        .file_name()
        .and_then(|v| v.to_str())
        .unwrap_or("key.toml");
    let backup = parent.join(format!(".{file_name}.emote-key-replace.bak"));
    let _ = fs::remove_file(&backup);
    fs::rename(path, &backup)
        .with_context(|| format!("move existing {} aside", path.display()))?;
    match fs::rename(temp_path, path) {
        Ok(()) => {
            let _ = fs::remove_file(&backup);
            Ok(())
        }
        Err(err) => {
            let restore = fs::rename(&backup, path);
            if let Err(restore_err) = restore {
                return Err(anyhow::anyhow!(
                    "replace {} failed: {}; restoring backup {} also failed: {}",
                    path.display(),
                    err,
                    backup.display(),
                    restore_err
                ));
            }
            Err(err).with_context(|| {
                format!(
                    "replace {} with temporary file {}",
                    path.display(),
                    temp_path.display()
                )
            })
        }
    }
}

fn parse_bytes_value(raw: &str, key: &str) -> Result<Option<Vec<u8>>> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Ok(None);
    }
    if raw.contains('[') {
        let (inner, _) = extract_bracketed(raw)
            .ok_or_else(|| anyhow::anyhow!("key.toml: {key} array missing closing ]"))?;
        return Ok(Some(parse_byte_array(inner, key)?));
    }
    if raw.starts_with('"') && raw.ends_with('"') && raw.len() >= 2 {
        let inner = &raw[1..raw.len() - 1];
        let bytes = angou::parse_hex_bytes(inner)
            .with_context(|| format!("key.toml: invalid hex for {key}"))?;
        return Ok(Some(bytes));
    }
    Ok(None)
}

fn parse_hex_value(raw: &str, key: &str) -> Result<Option<Vec<u8>>> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Ok(None);
    }
    let inner = raw.trim_matches('"');
    let bytes = angou::parse_hex_bytes(inner)
        .with_context(|| format!("key.toml: invalid hex for {key}"))?;
    Ok(Some(bytes))
}

fn parse_byte_array(inner: &str, key: &str) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    for part in inner.split(',') {
        let tok = part.trim();
        if tok.is_empty() {
            continue;
        }
        let value = if let Some(hex) = tok.strip_prefix("0x").or_else(|| tok.strip_prefix("0X")) {
            u8::from_str_radix(hex, 16)
                .with_context(|| format!("key.toml: invalid hex byte {tok}"))?
        } else {
            let v: u16 = tok
                .parse()
                .with_context(|| format!("key.toml: invalid byte {tok}"))?;
            if v > 0xFF {
                bail!("key.toml: byte out of range {tok}");
            }
            v as u8
        };
        bytes.push(value);
    }
    if bytes.is_empty() {
        bail!("key.toml: {key} array is empty");
    }
    Ok(bytes)
}

fn collect_rhs_for_key(text: &str, key: &str) -> Option<String> {
    let mut collecting = false;
    let mut out = String::new();

    for raw_line in text.lines() {
        let line = raw_line.split('#').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }

        if !collecting {
            let Some((lhs, rhs)) = line.split_once('=') else {
                continue;
            };
            if lhs.trim() != key {
                continue;
            }
            collecting = true;
            out.push_str(rhs.trim());
            if rhs.contains(']') {
                break;
            }
        } else {
            out.push(' ');
            out.push_str(line);
            if line.contains(']') {
                break;
            }
        }
    }

    if out.is_empty() {
        None
    } else {
        Some(out)
    }
}

fn extract_bracketed(raw: &str) -> Option<(&str, &str)> {
    let start = raw.find('[')?;
    let end = raw[start + 1..].find(']').map(|v| start + 1 + v)?;
    Some((&raw[start + 1..end], &raw[end + 1..]))
}

fn parse_chain_order(text: &str) -> Result<Option<Vec<AngouStepKind>>> {
    let Some(raw) = collect_rhs_for_key(text, "chain_order") else {
        return Ok(None);
    };
    let raw = raw.trim();
    if raw.is_empty() {
        return Ok(None);
    }
    let inner = if raw.contains('[') {
        let (inner, _) = extract_bracketed(raw)
            .ok_or_else(|| anyhow::anyhow!("key.toml: chain_order missing closing ]"))?;
        inner
    } else {
        raw
    };
    let mut out = Vec::new();
    for part in inner.split(',') {
        let tok = part.trim().trim_matches('"').trim_matches('\'');
        if tok.is_empty() {
            continue;
        }
        let kind = match tok.to_ascii_lowercase().as_str() {
            "exe" | "exe_key16" => AngouStepKind::ExeKey16,
            "base" | "base_code" => AngouStepKind::BaseCode,
            "game" | "game_code" => AngouStepKind::GameCode,
            other => bail!("key.toml: unknown chain_order item {other}"),
        };
        out.push(kind);
    }
    if out.is_empty() {
        return Ok(None);
    }
    Ok(Some(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_emote_key_hex_and_decimal() {
        let hex = parse_key_toml("emote_key = 0x89ABCDEF\n").unwrap();
        assert_eq!(hex.emote_key, Some(0x89AB_CDEF));

        let decimal = parse_key_toml("emote_key = 2309737967 # keep\n").unwrap();
        assert_eq!(decimal.emote_key, Some(0x89AB_CDEF));
    }

    #[test]
    fn updates_existing_emote_key_without_touching_other_lines() {
        let src = "key = [0x01, 0x02]\nemote_key = 123 # old\nchain_order = [\"exe\"]\n";
        let out = update_emote_key_toml_text(src, 0x1234_ABCD);
        assert_eq!(
            out,
            "key = [0x01, 0x02]\nemote_key = 305441741 # old\nchain_order = [\"exe\"]\n"
        );
    }

    #[test]
    fn appends_new_emote_key_and_preserves_crlf() {
        let src = "key_hex = \"00112233445566778899AABBCCDDEEFF\"\r\n";
        let out = update_emote_key_toml_text(src, 0x89AB_CDEF);
        assert_eq!(
            out,
            "key_hex = \"00112233445566778899AABBCCDDEEFF\"\r\nemote_key = 0x89ABCDEF\r\n"
        );
    }

    #[test]
    fn keeps_hex_style_for_existing_emote_key() {
        let src = "emote_key = 0x00000001\n";
        let out = update_emote_key_toml_text(src, 0x89AB_CDEF);
        assert_eq!(out, "emote_key = 0x89ABCDEF\n");
    }

    #[test]
    fn inserts_missing_emote_key_at_root_before_tables() {
        let src = "# project keys\n\n[legacy]\nvalue = 7\n";
        let out = update_emote_key_toml_text(src, 0x1234_ABCD);
        assert_eq!(
            out,
            "# project keys\n\nemote_key = 0x1234ABCD\n[legacy]\nvalue = 7\n"
        );
    }
}
