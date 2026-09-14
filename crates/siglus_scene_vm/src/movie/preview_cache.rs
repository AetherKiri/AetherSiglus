use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::assets::RgbaImage;

// Preview pixels are immutable file resources, unlike movie playbacks. Keep
// a small LRU across scene restarts without retaining streams or ImageIds.
const MAX_BYTES: usize = 32 * 1024 * 1024;
const MAX_ENTRIES: usize = 8;

#[derive(Debug, Default)]
pub(super) struct PreviewCache {
    entries: VecDeque<(PathBuf, Arc<RgbaImage>)>,
    bytes: usize,
}

impl PreviewCache {
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn get(&mut self, path: &Path) -> Option<Arc<RgbaImage>> {
        let index = self.entries.iter().position(|(key, _)| key == path)?;
        let entry = self.entries.remove(index)?;
        let image = Arc::clone(&entry.1);
        self.entries.push_back(entry);
        Some(image)
    }

    pub fn insert(&mut self, path: PathBuf, image: Arc<RgbaImage>) {
        self.insert_with_limits(path, image, MAX_BYTES, MAX_ENTRIES);
    }

    fn insert_with_limits(
        &mut self,
        path: PathBuf,
        image: Arc<RgbaImage>,
        max_bytes: usize,
        max_entries: usize,
    ) {
        if let Some(index) = self.entries.iter().position(|(key, _)| key == &path) {
            let (_, old) = self.entries.remove(index).unwrap();
            self.bytes -= old.rgba.len();
        }
        let size = image.rgba.len();
        if size > max_bytes || max_entries == 0 {
            return;
        }
        while self.bytes > max_bytes - size || self.entries.len() >= max_entries {
            let (_, old) = self.entries.pop_front().unwrap();
            self.bytes -= old.rgba.len();
        }
        self.bytes += size;
        self.entries.push_back((path, image));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn image(value: u8) -> Arc<RgbaImage> {
        Arc::new(RgbaImage {
            width: 1,
            height: 1,
            center_x: 0,
            center_y: 0,
            rgba: vec![value; 4],
        })
    }

    #[test]
    fn preview_cache_is_lru_bounded_and_replacement_accounts_bytes() {
        let mut cache = PreviewCache::default();
        let a = image(1);
        cache.insert_with_limits("a".into(), Arc::clone(&a), 8, 2);
        cache.insert_with_limits("b".into(), image(2), 8, 2);
        assert!(Arc::ptr_eq(&cache.get(Path::new("a")).unwrap(), &a));
        cache.insert_with_limits("c".into(), image(3), 8, 2);
        assert!(cache.get(Path::new("b")).is_none());
        assert_eq!(cache.bytes, 8);
        cache.insert_with_limits("a".into(), image(4), 8, 2);
        assert_eq!(cache.bytes, 8);
        assert_eq!(cache.get(Path::new("a")).unwrap().rgba, vec![4; 4]);
        cache.insert_with_limits("oversized".into(), image(5), 3, 2);
        assert!(cache.get(Path::new("oversized")).is_none());
    }

    #[test]
    fn scene_restart_keeps_preview_pixels_but_resets_playback_state() {
        let mut manager = super::super::MovieManager::new("preview-cache-test".into());
        let frame = image(7);
        let path = PathBuf::from("missing-but-cached.omv");
        manager
            .preview_cache
            .insert(path.clone(), Arc::clone(&frame));
        manager.current_append_dir = "append".into();
        manager.next_playback_id = 42;
        manager.reset_for_scene_restart();
        assert!(manager.current_append_dir.is_empty());
        assert_eq!(manager.next_playback_id, 1);
        assert!(manager.playbacks.is_empty());
        assert!(Arc::ptr_eq(
            &manager.ensure_preview_frame_for_path(path).unwrap(),
            &frame
        ));
    }
}
