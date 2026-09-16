use std::collections::HashMap;
use std::fs;
use std::io::Write;
use std::path::Path;
use std::sync::Arc;

use anyhow::{anyhow, bail, Context, Result};
use flate2::write::DeflateEncoder;
use flate2::Compression;

use crate::key_toml::StringEncryptionOverride;
use crate::lzss::lzss_unpack_lenient;

#[derive(Debug, Clone, Copy)]
pub struct CIndex {
    pub offset: i32,
    pub size: i32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PackIncProp {
    pub form: i32,
    pub size: i32,
}

impl PackIncProp {
    pub fn read(buf: &[u8], off: usize) -> Result<Self> {
        if off + 8 > buf.len() {
            bail!("scene_pck: PackIncProp out of bounds");
        }
        let form = i32::from_le_bytes(buf[off..off + 4].try_into().unwrap());
        let size = i32::from_le_bytes(buf[off + 4..off + 8].try_into().unwrap());
        Ok(Self { form, size })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PackIncCmd {
    pub scn_no: i32,
    pub offset: i32,
}

impl PackIncCmd {
    pub fn read(buf: &[u8], off: usize) -> Result<Self> {
        if off + 8 > buf.len() {
            bail!("scene_pck: PackIncCmd out of bounds");
        }
        let scn_no = i32::from_le_bytes(buf[off..off + 4].try_into().unwrap());
        let offset = i32::from_le_bytes(buf[off + 4..off + 8].try_into().unwrap());
        Ok(Self { scn_no, offset })
    }
}

impl CIndex {
    pub fn read(buf: &[u8], off: usize) -> Result<Self> {
        if off + 8 > buf.len() {
            bail!("scene_pck: CIndex out of bounds");
        }
        let offset = i32::from_le_bytes(buf[off..off + 4].try_into().unwrap());
        let size = i32::from_le_bytes(buf[off + 4..off + 8].try_into().unwrap());
        Ok(Self { offset, size })
    }
}

/// All fields are little-endian i32.
#[derive(Debug, Clone, Copy)]
pub struct PackScnHeader {
    pub header_size: i32,
    pub inc_prop_list_ofs: i32,
    pub inc_prop_cnt: i32,
    pub inc_prop_name_index_list_ofs: i32,
    pub inc_prop_name_index_cnt: i32,
    pub inc_prop_name_list_ofs: i32,
    pub inc_prop_name_cnt: i32,
    pub inc_cmd_list_ofs: i32,
    pub inc_cmd_cnt: i32,
    pub inc_cmd_name_index_list_ofs: i32,
    pub inc_cmd_name_index_cnt: i32,
    pub inc_cmd_name_list_ofs: i32,
    pub inc_cmd_name_cnt: i32,
    pub scn_name_index_list_ofs: i32,
    pub scn_name_index_cnt: i32,
    pub scn_name_list_ofs: i32,
    pub scn_name_cnt: i32,
    pub scn_data_index_list_ofs: i32,
    pub scn_data_index_cnt: i32,
    pub scn_data_list_ofs: i32,
    pub scn_data_cnt: i32,
    pub scn_data_exe_angou_mod: i32,
    pub original_source_header_size: i32,
}

impl PackScnHeader {
    pub fn read(buf: &[u8], off: usize, has_signature: bool) -> Result<Self> {
        // header size is stored in the first i32 (no signature in older builds).
        let min_need = if has_signature { 8 + 4 } else { 4 };
        if off + min_need > buf.len() {
            bail!("scene_pck: header out of bounds");
        }
        let mut p = off;
        if has_signature {
            if &buf[off..off + 8] != b"pack_scn" {
                bail!("scene_pck: bad signature (expected pack_scn)");
            }
            p += 8;
        }
        let mut rd = || {
            let v = i32::from_le_bytes(buf[p..p + 4].try_into().unwrap());
            p += 4;
            v
        };
        let header_size = rd();
        let mut out = Self {
            header_size,
            inc_prop_list_ofs: rd(),
            inc_prop_cnt: rd(),
            inc_prop_name_index_list_ofs: rd(),
            inc_prop_name_index_cnt: rd(),
            inc_prop_name_list_ofs: rd(),
            inc_prop_name_cnt: rd(),
            inc_cmd_list_ofs: rd(),
            inc_cmd_cnt: rd(),
            inc_cmd_name_index_list_ofs: rd(),
            inc_cmd_name_index_cnt: rd(),
            inc_cmd_name_list_ofs: rd(),
            inc_cmd_name_cnt: rd(),
            scn_name_index_list_ofs: rd(),
            scn_name_index_cnt: rd(),
            scn_name_list_ofs: rd(),
            scn_name_cnt: rd(),
            scn_data_index_list_ofs: rd(),
            scn_data_index_cnt: rd(),
            scn_data_list_ofs: rd(),
            scn_data_cnt: rd(),
            scn_data_exe_angou_mod: rd(),
            original_source_header_size: rd(),
        };

        // Optional extra fields in newer headers (ignored for now).
        let header_bytes = header_size.max(0) as usize;
        let base_fields_bytes = 23 * 4;
        let extra_bytes = header_bytes.saturating_sub(base_fields_bytes);
        let extra_fields = extra_bytes / 4;
        if extra_fields > 0 {
            for _ in 0..extra_fields {
                let _ = rd();
            }
        }

        Ok(out)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SceneStringCodec {
    Plain,
    Xor,
}

#[derive(Debug, Clone)]
pub struct ScenePckDecodeOptions {
    /// Optional 16-byte exe angou element table (`TNM_EXE_ANGOU_ELEMENT_CNT`).
    pub exe_angou_element: Option<Vec<u8>>,
    /// Optional easy angou code table (`TNM_EASY_ANGOU_CODE_SIZE`, typically 256).
    pub easy_angou_code: Option<Vec<u8>>,
    /// Project override for the historical scene string-table codec.
    pub string_encryption_override: StringEncryptionOverride,
}

impl Default for ScenePckDecodeOptions {
    fn default() -> Self {
        Self {
            exe_angou_element: None,
            easy_angou_code: None,
            string_encryption_override: StringEncryptionOverride::Mdl,
        }
    }
}

impl ScenePckDecodeOptions {
    pub fn from_project_dir(project_dir: &Path) -> Result<Self> {
        let cfg = crate::key_toml::load_key_toml_from_project_dir(project_dir)?;
        let exe = cfg
            .as_ref()
            .and_then(|cfg| cfg.exe_key16)
            .map(|v| v.to_vec());
        let string_encryption_override = cfg
            .map(|cfg| cfg.override_string_encryption)
            .unwrap_or(StringEncryptionOverride::Mdl);
        Ok(Self {
            exe_angou_element: exe,
            easy_angou_code: Some(crate::keys::SCENE_KEY.to_vec()),
            string_encryption_override,
        })
    }
}

#[derive(Debug, Clone)]
pub struct ScenePck {
    pub buf: Vec<u8>,
    pub header: PackScnHeader,
    pub scn_name_map: HashMap<String, usize>,
    pub inc_prop_name_map: HashMap<u32, String>,
    /// Immutable include-command names shared by scene execution cursors.
    pub inc_cmd_name_map: Arc<HashMap<u32, String>>,
    pub inc_props: Vec<PackIncProp>,
    pub inc_cmds: Vec<PackIncCmd>,
    pub string_codec: SceneStringCodec,
}

fn read_pack_inc_props(buf: &[u8], list_ofs: usize, count: usize) -> Result<Vec<PackIncProp>> {
    let mut out = Vec::new();
    if count == 0 {
        return Ok(out);
    }
    let byte_len = count
        .checked_mul(8)
        .ok_or_else(|| anyhow!("scene_pck: inc_prop_list size overflow"))?;
    let end = list_ofs
        .checked_add(byte_len)
        .ok_or_else(|| anyhow!("scene_pck: inc_prop_list offset overflow"))?;
    if end > buf.len() {
        bail!("scene_pck: inc_prop_list out of bounds");
    }
    out.reserve(count);
    for i in 0..count {
        out.push(PackIncProp::read(buf, list_ofs + i * 8)?);
    }
    Ok(out)
}

fn read_pack_inc_cmds(buf: &[u8], list_ofs: usize, count: usize) -> Result<Vec<PackIncCmd>> {
    let mut out = Vec::new();
    if count == 0 {
        return Ok(out);
    }
    let byte_len = count
        .checked_mul(8)
        .ok_or_else(|| anyhow!("scene_pck: inc_cmd_list size overflow"))?;
    let end = list_ofs
        .checked_add(byte_len)
        .ok_or_else(|| anyhow!("scene_pck: inc_cmd_list offset overflow"))?;
    if end > buf.len() {
        bail!("scene_pck: inc_cmd_list out of bounds");
    }
    out.reserve(count);
    for i in 0..count {
        out.push(PackIncCmd::read(buf, list_ofs + i * 8)?);
    }
    Ok(out)
}

fn read_indexed_utf16_name_map(
    buf: &[u8],
    index_list_ofs: usize,
    count: usize,
    list_ofs: usize,
) -> Result<HashMap<u32, String>> {
    let mut out = HashMap::new();
    if index_list_ofs + count * 8 > buf.len() || list_ofs > buf.len() {
        return Ok(out);
    }
    for i in 0..count {
        let idx = CIndex::read(buf, index_list_ofs + i * 8)?;
        let o = idx.offset.max(0) as usize;
        let n = idx.size.max(0) as usize;
        let byte_off = list_ofs
            .checked_add(o * 2)
            .ok_or_else(|| anyhow!("scene_pck: name offset overflow"))?;
        let byte_end = byte_off
            .checked_add(n * 2)
            .ok_or_else(|| anyhow!("scene_pck: name size overflow"))?;
        if byte_end > buf.len() {
            continue;
        }
        let mut u16s = Vec::with_capacity(n);
        for j in 0..n {
            let p = byte_off + j * 2;
            let w = u16::from_le_bytes([buf[p], buf[p + 1]]);
            if w == 0 {
                break;
            }
            u16s.push(w);
        }
        let s = String::from_utf16_lossy(&u16s);
        if !s.is_empty() {
            out.insert(i as u32, s);
        }
    }
    Ok(out)
}


fn read_scene_string_header(chunk: &[u8]) -> Result<(usize, usize, usize)> {
    if chunk.len() < 28 {
        bail!("scene_pck: scene chunk too short for string header");
    }
    let rd = |off: usize| -> i32 {
        i32::from_le_bytes(chunk[off..off + 4].try_into().unwrap())
    };
    let str_index_list_ofs = rd(12);
    let str_index_cnt = rd(16);
    let str_list_ofs = rd(20);
    if str_index_list_ofs < 0 || str_index_cnt < 0 || str_list_ofs < 0 {
        bail!("scene_pck: negative scene string-table field");
    }
    Ok((
        str_index_list_ofs as usize,
        str_index_cnt as usize,
        str_list_ofs as usize,
    ))
}

fn append_mdl_string_candidates(
    plain: &mut Vec<u8>,
    xor: &mut Vec<u8>,
    chunk: &[u8],
    index_list_ofs: usize,
    str_list_ofs: usize,
    str_id: usize,
) -> Result<usize> {
    let idx = CIndex::read(chunk, index_list_ofs + str_id * 8)?;
    if idx.offset < 0 || idx.size < 0 {
        bail!("scene_pck: negative scene string index");
    }
    let units = idx.size as usize;
    let byte_off = str_list_ofs
        .checked_add((idx.offset as usize).checked_mul(2).ok_or_else(|| anyhow!("scene_pck: string offset overflow"))?)
        .ok_or_else(|| anyhow!("scene_pck: string offset overflow"))?;
    let byte_end = byte_off
        .checked_add(units.checked_mul(2).ok_or_else(|| anyhow!("scene_pck: string size overflow"))?)
        .ok_or_else(|| anyhow!("scene_pck: string size overflow"))?;
    if byte_end > chunk.len() {
        bail!("scene_pck: scene string data out of bounds");
    }

    let units_bytes = (units as u32).to_le_bytes();
    plain.extend_from_slice(&units_bytes);
    xor.extend_from_slice(&units_bytes);
    let key = (28807u32).wrapping_mul(str_id as u32) as u16;
    for unit_no in 0..units {
        let pos = byte_off + unit_no * 2;
        let raw = u16::from_le_bytes([chunk[pos], chunk[pos + 1]]);
        plain.extend_from_slice(&raw.to_le_bytes());
        xor.extend_from_slice(&(raw ^ key).to_le_bytes());
    }
    Ok(units)
}

fn mdl_deflated_len(input: &[u8]) -> Result<usize> {
    let mut encoder = DeflateEncoder::new(Vec::new(), Compression::best());
    encoder.write_all(input)?;
    Ok(encoder.finish()?.len())
}

fn detect_scene_string_codec_mdl(buf: &[u8], header: &PackScnHeader) -> Result<SceneStringCodec> {
    let mut plain = Vec::new();
    let mut xor = Vec::new();
    let scn_cnt = header
        .scn_data_cnt
        .max(header.scn_data_index_cnt)
        .max(0) as usize;
    let idx_ofs = header.scn_data_index_list_ofs.max(0) as usize;
    let data_base = header.scn_data_list_ofs.max(0) as usize;
    let mut total_units = 0usize;

    for scn_no in 0..scn_cnt {
        let idx = CIndex::read(buf, idx_ofs + scn_no * 8)?;
        if idx.size <= 0 {
            continue;
        }
        let chunk_start = data_base
            .checked_add(idx.offset.max(0) as usize)
            .ok_or_else(|| anyhow!("scene_pck: scene offset overflow during MDL detection"))?;
        let chunk_end = chunk_start
            .checked_add(idx.size as usize)
            .ok_or_else(|| anyhow!("scene_pck: scene size overflow during MDL detection"))?;
        let chunk = buf
            .get(chunk_start..chunk_end)
            .ok_or_else(|| anyhow!("scene_pck: scene out of bounds during MDL detection"))?;
        let (string_index_ofs, string_count, string_list_ofs) =
            match read_scene_string_header(chunk) {
                Ok(v) => v,
                Err(_) => continue,
            };
        let index_end = string_index_ofs
            .checked_add(string_count.checked_mul(8).ok_or_else(|| anyhow!("scene_pck: string index size overflow"))?)
            .ok_or_else(|| anyhow!("scene_pck: string index offset overflow"))?;
        if index_end > chunk.len() || string_list_ofs > chunk.len() {
            continue;
        }

        // Identical scene/string framing is written to both candidates. The
        // only difference is the candidate decoding of each UTF-16 code unit.
        plain.extend_from_slice(&(scn_no as u32).to_le_bytes());
        xor.extend_from_slice(&(scn_no as u32).to_le_bytes());
        for str_id in 0..string_count {
            plain.extend_from_slice(&(str_id as u32).to_le_bytes());
            xor.extend_from_slice(&(str_id as u32).to_le_bytes());
            total_units = total_units.saturating_add(append_mdl_string_candidates(
                &mut plain,
                &mut xor,
                chunk,
                string_index_ofs,
                string_list_ofs,
                str_id,
            )?);
        }
    }

    if total_units == 0 {
        return Ok(SceneStringCodec::Xor);
    }
    let plain_len = mdl_deflated_len(&plain)?;
    let xor_len = mdl_deflated_len(&xor)?;
    Ok(if plain_len < xor_len {
        SceneStringCodec::Plain
    } else {
        // Preserve the historical runtime behavior on ties or weak/noisy
        // corpora; MDL must strictly prefer Plain before we disable XOR.
        SceneStringCodec::Xor
    })
}

fn resolve_scene_string_codec(
    buf: &[u8],
    header: &PackScnHeader,
    mode: StringEncryptionOverride,
) -> Result<SceneStringCodec> {
    match mode {
        StringEncryptionOverride::Xor => Ok(SceneStringCodec::Xor),
        StringEncryptionOverride::None => Ok(SceneStringCodec::Plain),
        StringEncryptionOverride::Mdl => detect_scene_string_codec_mdl(buf, header),
    }
}

impl ScenePck {
    pub fn load_and_rebuild(path: &Path, opt: &ScenePckDecodeOptions) -> Result<Self> {
        let tmp = fs::read(path).with_context(|| format!("read {}", path.display()))?;
        Self::load_and_rebuild_from_bytes(tmp, opt)
    }

    pub fn load_and_rebuild_from_bytes(mut tmp: Vec<u8>, opt: &ScenePckDecodeOptions) -> Result<Self> {
        if tmp.len() < 4 {
            bail!("scene_pck: file too short");
        }
        let has_signature = tmp.len() >= 8 && &tmp[0..8] == b"pack_scn";
        let header = PackScnHeader::read(&tmp, 0, has_signature)?;
        let scn_data_list_ofs = header.scn_data_list_ofs as usize;
        if scn_data_list_ofs > tmp.len() {
            bail!("scene_pck: scn_data_list_ofs out of bounds");
        }

        // Rebuild m_scn_data exactly like the original implementation: keep everything before scn_data_list_ofs,
        // then append decrypted/decompressed scene chunks contiguously.
        let mut out = tmp[..scn_data_list_ofs].to_vec();

        // Load original index list from the input.
        let idx_ofs = header.scn_data_index_list_ofs as usize;
        let scn_cnt = if header.scn_data_cnt > 0 {
            header.scn_data_cnt as usize
        } else {
            header.scn_data_index_cnt.max(0) as usize
        };
        if idx_ofs + scn_cnt * 8 > tmp.len() {
            bail!("scene_pck: scn_data_index_list out of bounds");
        }
        let mut idx_list: Vec<CIndex> = Vec::with_capacity(scn_cnt);
        for i in 0..scn_cnt {
            idx_list.push(CIndex::read(&tmp, idx_ofs + i * 8)?);
        }

        let mut offset = idx_list
            .get(0)
            .map(|x| x.offset.max(0) as usize)
            .unwrap_or(0);
        if out.len() < scn_data_list_ofs + offset {
            out.resize(scn_data_list_ofs + offset, 0);
        }

        for scn_no in 0..scn_cnt {
            let entry = idx_list[scn_no];
            let mut new_size = 0usize;

            if entry.size > 0 {
                let sp_off = scn_data_list_ofs
                    .checked_add(entry.offset.max(0) as usize)
                    .ok_or_else(|| anyhow!("scene_pck: offset overflow"))?;
                let sp_end = sp_off
                    .checked_add(entry.size as usize)
                    .ok_or_else(|| anyhow!("scene_pck: size overflow"))?;
                if sp_end > tmp.len() {
                    bail!(
                        "scene_pck: scn chunk out of bounds (scn_no={}, end={}, len={})",
                        scn_no,
                        sp_end,
                        tmp.len()
                    );
                }

                let chunk = &mut tmp[sp_off..sp_end];

                let out_chunk: Vec<u8>;
                if header.original_source_header_size > 0 {
                    // exe angou element XOR (optional)
                    if header.scn_data_exe_angou_mod != 0 {
                        if let Some(exe_el) = opt.exe_angou_element.as_deref() {
                            if exe_el.is_empty() {
                                // nothing
                            } else {
                                let mut eac = 0usize;
                                for b in chunk.iter_mut() {
                                    *b ^= exe_el[eac];
                                    eac += 1;
                                    if eac >= exe_el.len() {
                                        eac = 0;
                                    }
                                }
                            }
                        }
                    }

                    // easy angou XOR (optional)
                    if let Some(easy) = opt.easy_angou_code.as_deref() {
                        if !easy.is_empty() {
                            let mut eac = 0usize;
                            for b in chunk.iter_mut() {
                                *b ^= easy[eac];
                                eac += 1;
                                if eac >= easy.len() {
                                    eac = 0;
                                }
                            }
                        }
                    }

                    out_chunk = lzss_unpack_lenient(chunk)
                        .with_context(|| format!("scene_pck: lzss unpack scn_no={}", scn_no))?;
                } else {
                    // Easy-link mode: keep the chunk bytes as-is.
                    out_chunk = chunk.to_vec();
                }

                new_size = out_chunk.len();
                let dst_off = scn_data_list_ofs + offset;
                let need_len = dst_off
                    .checked_add(new_size)
                    .ok_or_else(|| anyhow!("scene_pck: out size overflow"))?;
                if out.len() < need_len {
                    out.resize(need_len, 0);
                }
                out[dst_off..dst_off + new_size].copy_from_slice(&out_chunk);
            }

            // Patch the index list inside the output buffer.
            let out_idx_ofs = header.scn_data_index_list_ofs as usize;
            let out_entry_ofs = out_idx_ofs + scn_no * 8;
            if out_entry_ofs + 8 > out.len() {
                bail!("scene_pck: output index list out of bounds");
            }
            out[out_entry_ofs..out_entry_ofs + 4].copy_from_slice(&(offset as i32).to_le_bytes());
            out[out_entry_ofs + 4..out_entry_ofs + 8]
                .copy_from_slice(&(new_size as i32).to_le_bytes());

            offset = offset
                .checked_add(new_size)
                .ok_or_else(|| anyhow!("scene_pck: offset overflow"))?;
        }

        // Build name map.
        let mut scn_name_map = HashMap::new();
        let name_idx_ofs = header.scn_name_index_list_ofs as usize;
        let name_cnt = header.scn_name_cnt.max(0) as usize;
        let name_list_ofs = header.scn_name_list_ofs as usize;
        if name_idx_ofs + name_cnt * 8 <= out.len() && name_list_ofs <= out.len() {
            for i in 0..name_cnt {
                let idx = CIndex::read(&out, name_idx_ofs + i * 8)?;
                let o = idx.offset.max(0) as usize;
                let n = idx.size.max(0) as usize;
                let byte_off = name_list_ofs
                    .checked_add(o * 2)
                    .ok_or_else(|| anyhow!("scene_pck: name offset overflow"))?;
                let byte_end = byte_off
                    .checked_add(n * 2)
                    .ok_or_else(|| anyhow!("scene_pck: name size overflow"))?;
                if byte_end > out.len() {
                    continue;
                }
                let mut u16s = Vec::with_capacity(n);
                for j in 0..n {
                    let p = byte_off + j * 2;
                    let w = u16::from_le_bytes([out[p], out[p + 1]]);
                    if w == 0 {
                        break;
                    }
                    u16s.push(w);
                }
                let s = String::from_utf16_lossy(&u16s);
                if !s.is_empty() {
                    scn_name_map.insert(s, i);
                }
            }
        }

        let inc_prop_name_map = read_indexed_utf16_name_map(
            &out,
            header.inc_prop_name_index_list_ofs.max(0) as usize,
            header.inc_prop_name_cnt.max(0) as usize,
            header.inc_prop_name_list_ofs.max(0) as usize,
        )?;
        let inc_cmd_name_map = read_indexed_utf16_name_map(
            &out,
            header.inc_cmd_name_index_list_ofs.max(0) as usize,
            header.inc_cmd_name_cnt.max(0) as usize,
            header.inc_cmd_name_list_ofs.max(0) as usize,
        )?;
        let inc_props = read_pack_inc_props(
            &out,
            header.inc_prop_list_ofs.max(0) as usize,
            header.inc_prop_cnt.max(0) as usize,
        )?;
        let inc_cmds = read_pack_inc_cmds(
            &out,
            header.inc_cmd_list_ofs.max(0) as usize,
            header.inc_cmd_cnt.max(0) as usize,
        )?;
        let string_codec = resolve_scene_string_codec(
            &out,
            &header,
            opt.string_encryption_override,
        )?;

        Ok(Self {
            buf: out,
            header,
            scn_name_map,
            inc_prop_name_map,
            inc_cmd_name_map: Arc::new(inc_cmd_name_map),
            inc_props,
            inc_cmds,
            string_codec,
        })
    }

    pub fn scn_data_slice(&self, scn_no: usize) -> Result<&[u8]> {
        let scn_cnt = self.header.scn_data_cnt.max(0) as usize;
        if scn_no >= scn_cnt {
            bail!("scene_pck: scn_no out of range");
        }
        let idx_ofs = self.header.scn_data_index_list_ofs as usize;
        let entry = CIndex::read(&self.buf, idx_ofs + scn_no * 8)?;
        if entry.size <= 0 {
            return Ok(&[]);
        }
        let base = self.header.scn_data_list_ofs as usize;
        let off = base
            .checked_add(entry.offset.max(0) as usize)
            .ok_or_else(|| anyhow!("scene_pck: offset overflow"))?;
        let end = off
            .checked_add(entry.size as usize)
            .ok_or_else(|| anyhow!("scene_pck: size overflow"))?;
        if end > self.buf.len() {
            bail!("scene_pck: scn slice out of bounds");
        }
        Ok(&self.buf[off..end])
    }

    pub fn find_scene_no(&self, name_or_index: &str) -> Option<usize> {
        if let Ok(i) = name_or_index.parse::<usize>() {
            return Some(i);
        }
        self.scn_name_map.get(name_or_index).copied().or_else(|| {
            // Script scene names are case-insensitive, just like named user
            // commands. Compiled packs can store a lowercased name while a
            // jump/farcall operand retains the source spelling.
            self.scn_name_map
                .iter()
                .filter(|(name, _)| name.eq_ignore_ascii_case(name_or_index))
                .map(|(_, no)| *no)
                .min()
        })
    }

    pub fn find_scene_name(&self, scn_no: usize) -> Option<&str> {
        self.scn_name_map.iter().find_map(|(name, no)| {
            if *no == scn_no {
                Some(name.as_str())
            } else {
                None
            }
        })
    }

    pub fn find_inc_cmd_no(&self, cmd_name: &str) -> Option<usize> {
        self.inc_cmd_name_map.iter().find_map(|(no, name)| {
            if name.eq_ignore_ascii_case(cmd_name) {
                Some(*no as usize)
            } else {
                None
            }
        })
    }
}

fn find_child_case_insensitive(parent: &Path, name: &str, want_dir: bool) -> Result<Option<std::path::PathBuf>> {
    let exact = parent.join(name);
    if (want_dir && exact.is_dir()) || (!want_dir && exact.is_file()) {
        return Ok(Some(exact));
    }

    let entries = match std::fs::read_dir(parent) {
        Ok(entries) => entries,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(err).with_context(|| format!("read_dir {}", parent.display())),
    };
    let mut matches = Vec::new();
    for entry in entries {
        let entry = entry?;
        if !entry
            .file_name()
            .to_string_lossy()
            .eq_ignore_ascii_case(name)
        {
            continue;
        }
        let path = entry.path();
        if (want_dir && path.is_dir()) || (!want_dir && path.is_file()) {
            matches.push(path);
        }
    }
    matches.sort();
    match matches.len() {
        0 => Ok(None),
        1 => Ok(matches.pop()),
        _ => bail!(
            "scene_pck: case-insensitive path conflict for {} under {}: {}",
            name,
            parent.display(),
            matches
                .iter()
                .map(|p| p.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

/// Helper for typical game directory layout using the Windows-style
/// case-insensitive path semantics expected by Siglus.
pub fn find_scene_pck_in_project(project_dir: &Path) -> Result<std::path::PathBuf> {
    if let Some(path) = find_child_case_insensitive(project_dir, "Scene.pck", false)? {
        return Ok(path);
    }
    if let Some(data_dir) = find_child_case_insensitive(project_dir, "Data", true)? {
        if let Some(path) = find_child_case_insensitive(&data_dir, "Scene.pck", false)? {
            return Ok(path);
        }
    }
    bail!(
        "scene_pck: Scene.pck not found under {}",
        project_dir.display()
    );
}

#[cfg(test)]
mod scene_name_tests {
    use super::*;

    fn pack_with_names(names: &[(&str, usize)]) -> ScenePck {
        ScenePck {
            buf: Vec::new(),
            header: PackScnHeader::read(&[0; 23 * 4], 0, false).unwrap(),
            scn_name_map: names.iter().map(|(name, no)| (name.to_string(), *no)).collect(),
            inc_prop_name_map: HashMap::new(),
            inc_cmd_name_map: Arc::default(),
            inc_props: Vec::new(),
            inc_cmds: Vec::new(),
            string_codec: SceneStringCodec::Xor,
        }
    }

    #[test]
    fn scene_lookup_matches_script_names_without_ascii_case() {
        let pack = pack_with_names(&[("_title_menu", 2), ("背景_test", 3)]);
        assert_eq!(pack.find_scene_no("_TITLE_Menu"), Some(2));
        assert_eq!(pack.find_scene_no("背景_TEST"), Some(3));
        assert_eq!(pack.find_scene_no("missing"), None);
        assert_eq!(pack.find_scene_no("12"), Some(12));
        assert_eq!(pack.find_scene_name(2), Some("_title_menu"));
    }

    #[test]
    fn scene_lookup_preserves_exact_match_and_stable_fallback() {
        let pack = pack_with_names(&[("Title", 7), ("TITLE", 4)]);
        assert_eq!(pack.find_scene_no("Title"), Some(7));
        assert_eq!(pack.find_scene_no("TITLE"), Some(4));
        assert_eq!(pack.find_scene_no("title"), Some(4));
    }

    #[test]
    fn mdl_candidates_preserve_plain_and_xor_byte_streams() {
        let mut chunk = vec![0u8; 30];
        chunk[20..24].copy_from_slice(&3i32.to_le_bytes());
        chunk[24..30].copy_from_slice(&[0x41, 0, 0x42, 0, 0x43, 0]);
        let mut plain = Vec::new();
        let mut xor = Vec::new();
        assert_eq!(
            append_mdl_string_candidates(&mut plain, &mut xor, &chunk, 0, 24, 2).unwrap(),
            3
        );
        let key = (28807u32.wrapping_mul(2)) as u16;
        let mut expected_xor = 3u32.to_le_bytes().to_vec();
        for raw in [0x41u16, 0x42, 0x43] {
            expected_xor.extend_from_slice(&(raw ^ key).to_le_bytes());
        }
        assert_eq!(plain, [3u32.to_le_bytes().as_slice(), &[0x41, 0, 0x42, 0, 0x43, 0]].concat());
        assert_eq!(xor, expected_xor);
    }
}
