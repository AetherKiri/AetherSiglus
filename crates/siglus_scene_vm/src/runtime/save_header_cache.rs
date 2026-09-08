//! Save-menu header cache. Missing slots are indexed by one directory scan,
//! not by hundreds of case-insensitive path walks on every script frame.
use super::globals::SaveSlotState;
use crate::original_save;
use std::path::Path;

#[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
use std::{collections::BTreeMap, fs, path::PathBuf, time::SystemTime};

#[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
#[derive(PartialEq, Eq)]
struct Stamp {
    len: u64,
    modified: Option<SystemTime>,
    // Detect in-place replacement even when a tool preserves mtime and size.
    #[cfg(unix)]
    changed: (i64, i64),
}

#[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
impl Stamp {
    fn read(metadata: &fs::Metadata) -> Self {
        #[cfg(unix)]
        use std::os::unix::fs::MetadataExt;
        Self {
            len: metadata.len(),
            modified: metadata.modified().ok(),
            #[cfg(unix)]
            changed: (metadata.ctime(), metadata.ctime_nsec()),
        }
    }
}

#[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
struct Entry {
    path: PathBuf,
    stamp: Option<Stamp>,
    slot: SaveSlotState,
}

#[derive(Default)]
pub(crate) struct SaveHeaderCache {
    #[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
    directory: PathBuf,
    #[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
    directory_stamp: Option<Stamp>,
    #[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
    entries: BTreeMap<usize, Entry>,
    #[cfg(test)]
    reads: usize,
}

impl SaveHeaderCache {
    pub(crate) fn slots(&mut self, project: &Path, start: usize, count: usize,
    ) -> Vec<SaveSlotState> {
        #[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
        {
            return (start..start.saturating_add(count)).map(|index| {
                original_save::read_slot_from_path(&original_save::save_file_path_for_no(project, index,
                    ))
                    .unwrap_or_default()
            }).collect();
        }
        #[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
        {
            let directory = original_save::save_dir(project);
            if self.refresh_directory(&directory).is_err() {
                // Do not keep showing stale saves after deletion or loss of
                // access, and retry on the next query (no sticky error cache).
                self.entries.clear();
                self.directory_stamp = None;
            }
            (start..start.saturating_add(count)).map(|index| {
                let Some(entry) = self.entries.get_mut(&index) else { return SaveSlotState::default(); };
                let Ok(metadata) = fs::metadata(&entry.path) else {
                    entry.stamp = None;
                    entry.slot = SaveSlotState::default();
                    return entry.slot.clone();
                };
                let stamp = Stamp::read(&metadata);
                if entry.stamp.as_ref() != Some(&stamp) || stamp.modified.is_none() {
                    entry.slot = original_save::read_slot_from_path(&entry.path).unwrap_or_default();
                    entry.stamp = Some(stamp);
                    #[cfg(test)]
                    { self.reads += 1; }
                }
                entry.slot.clone()
            }).collect()
        }
    }

    #[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
    fn refresh_directory(&mut self, requested: &Path) -> anyhow::Result<()> {
        let directory = crate::resource::resolve_windows_case_insensitive_path(requested)?
            .ok_or_else(|| anyhow::anyhow!("save directory unavailable"))?;
        let stamp = Stamp::read(&fs::metadata(&directory)?);
        if self.directory == directory && self.directory_stamp.as_ref() == Some(&stamp)
            && stamp.modified.is_some() { return Ok(()); }
        let mut files = BTreeMap::new();
        for file in fs::read_dir(&directory)? {
            let file = file?;
            let name = file.file_name();
            let name = name.to_string_lossy();
            let Some((stem, extension)) = name.rsplit_once('.') else { continue; };
            let Ok(index) = stem.parse::<usize>() else { continue; };
            if !extension.eq_ignore_ascii_case("sav") || !name.eq_ignore_ascii_case(&format!("{index:04}.sav")) {
                continue;
            }
            // Match the resolver's preference for the exact canonical case.
            if !files.contains_key(&index) || name == format!("{index:04}.sav") {
                files.insert(index, file.path());
            }
        }
        let mut previous = if self.directory == directory { std::mem::take(&mut self.entries) }
            else { BTreeMap::new() };
        self.entries = files.into_iter().map(|(index, path)| {
            let entry = previous.remove(&index).filter(|entry| entry.path == path)
                .unwrap_or_else(|| Entry { path, stamp: None, slot: SaveSlotState::default(),
                    });
            (index, entry)
        }).collect();
        self.directory = directory;
        self.directory_stamp = Some(stamp);
        Ok(())
    }
}

#[cfg(all(test, not(all(target_arch = "wasm32", target_os = "unknown"))))]
mod tests {
    use super::*;
    use crate::original_save::OriginalSaveHeader;
    use std::sync::atomic::{AtomicU64, Ordering};

    struct Project(PathBuf);
    impl Project {
        fn new() -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let path = std::env::temp_dir().join(format!("siglus-save-index-{}-{}-{}",
                std::process::id(), SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).unwrap().as_nanos(),
                NEXT.fetch_add(1, Ordering::Relaxed)));
            fs::create_dir(&path).unwrap();
            fs::create_dir(path.join("savedata")).unwrap();
            Self(path)
        }
        fn write(&self, name: &str, year: i32, extra: usize) {
            let header = OriginalSaveHeader { year, title: "中文 / 日本語".into(), ..Default::default() };
            let mut bytes = header.to_bytes();
            bytes.resize(bytes.len() + extra, 0xab);
            fs::write(self.0.join("savedata").join(name), bytes).unwrap();
        }
    }
    impl Drop for Project {
        fn drop(&mut self) { let _ = fs::remove_dir_all(&self.0); }
    }

    #[test]
    fn unchanged_headers_and_missing_slots_do_not_repeat_reads() {
        let project = Project::new();
        project.write("0000.sav", 2026, 1_200_000);
        let mut cache = SaveHeaderCache::default();
        for _ in 0..30 {
            let slots = cache.slots(&project.0, 0, 200);
            assert_eq!(slots.len(), 200);
            assert!(slots[0].exist);
            assert_eq!(slots[0].title, "中文 / 日本語");
            assert!(slots[1..].iter().all(|slot| !slot.exist));
        }
        assert_eq!(cache.reads, 1);
    }

    #[test]
    fn external_create_overwrite_delete_rename_and_quick_offsets_refresh() {
        let project = Project::new();
        let mut cache = SaveHeaderCache::default();
        assert!(!cache.slots(&project.0, 0, 3)[0].exist);
        project.write("0000.sav", 2025, 0);
        assert_eq!(cache.slots(&project.0, 0, 3)[0].year, 2025);
        project.write("0000.sav", 2026, 1);
        assert_eq!(cache.slots(&project.0, 0, 3)[0].year, 2026);
        project.write("0002.SAV", 2024, 0);
        assert!(cache.slots(&project.0, 0, 3)[2].exist);
        fs::rename(project.0.join("savedata/0002.SAV"), project.0.join("savedata/0200.sav"),
        ).unwrap();
        assert!(!cache.slots(&project.0, 0, 3)[2].exist);
        assert_eq!(cache.slots(&project.0, 200, 2)[0].year, 2024);
        fs::remove_file(project.0.join("savedata/0000.sav")).unwrap();
        assert!(!cache.slots(&project.0, 0, 3)[0].exist);
    }

    #[test]
    fn corrupt_headers_are_cached_but_replacement_is_not_sticky() {
        let project = Project::new();
        fs::write(project.0.join("savedata/0000.sav"), b"short").unwrap();
        let mut cache = SaveHeaderCache::default();
        for _ in 0..3 { assert!(!cache.slots(&project.0, 0, 1)[0].exist); }
        assert_eq!(cache.reads, 1);
        project.write("0000.sav", 2026, 0);
        assert!(cache.slots(&project.0, 0, 1)[0].exist);
        assert_eq!(cache.reads, 2);
        let other = Project::new();
        assert!(!cache.slots(&other.0, 0, 1)[0].exist);
        assert!(cache.slots(&project.0, 0, 1)[0].exist);
    }

    #[test]
    #[ignore = "read-only local save-index benchmark; set SIGLUS_SAVE_BENCH_PROJECT"]
    fn profile_local_save_index() {
        let project = PathBuf::from(std::env::var_os("SIGLUS_SAVE_BENCH_PROJECT").expect("project path"));
        let mut cache = SaveHeaderCache::default();
        let start = std::time::Instant::now();
        for _ in 0..100 { std::hint::black_box(cache.slots(&project, 0, 200)); }
        eprintln!("save_index: 100 queries {:.3} ms, header reads {}", start.elapsed().as_secs_f64() * 1000.0, cache.reads);
    }
}
