//! Immutable scene information shared by the VM and global-save operations.

use anyhow::Result;
use siglus_assets::scene_pck::ScenePck;
use std::collections::HashMap;

use crate::scene_stream::ScnHeader;

#[derive(Debug)]
pub(crate) struct SceneMetadata {
    pub rows: Vec<(String, usize)>,
    names: HashMap<String, usize>,
}

impl SceneMetadata {
    pub fn from_pack(pck: &ScenePck) -> Result<Self> {
        let count = pck.header.scn_data_cnt.max(0) as usize;
        let mut rows = Vec::with_capacity(count);
        for scene_no in 0..count {
            let name = pck
                .find_scene_name(scene_no)
                .map(ToOwned::to_owned)
                .unwrap_or_else(|| scene_no.to_string());
            let chunk = pck.scn_data_slice(scene_no)?;
            let flags = if chunk.is_empty() {
                0
            } else {
                ScnHeader::read(chunk)?.read_flag_cnt.max(0) as usize
            };
            rows.push((name, flags));
        }
        let mut metadata = Self::from_rows(rows);
        metadata.names = pck.scn_name_map.clone();
        Ok(metadata)
    }

    pub fn from_rows(rows: Vec<(String, usize)>) -> Self {
        let names = rows
            .iter()
            .enumerate()
            .map(|(no, (name, _))| (name.clone(), no))
            .collect();
        Self { rows, names }
    }

    pub fn find_scene_no(&self, name: &str) -> Option<usize> {
        if let Ok(no) = name.parse::<usize>() {
            return Some(no);
        }
        self.names.get(name).copied().or_else(|| {
            self.names
                .iter()
                .filter(|(candidate, _)| candidate.eq_ignore_ascii_case(name))
                .map(|(_, no)| *no)
                .min()
        })
    }
}
