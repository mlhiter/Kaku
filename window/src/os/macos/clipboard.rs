use crate::macos::{nsstring, nsstring_to_str};
use crate::ClipboardData;
use cocoa::appkit::{NSFilenamesPboardType, NSPasteboard, NSStringPboardType};
use cocoa::base::*;
use cocoa::foundation::NSArray;
use objc::*;
#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const PNG_PASTEBOARD_TYPE: &str = "public.png";
const TIFF_PASTEBOARD_TYPE: &str = "public.tiff";
const MAX_CLIPBOARD_IMAGE_BYTES: usize = 32 * 1024 * 1024;
const CLIPBOARD_IMAGE_DIR: &str = "clipboard-images";
const CLIPBOARD_IMAGE_FILE_PREFIX: &str = "clipboard-image-";
const MAX_CLIPBOARD_IMAGE_FILES: usize = 128;
const CLIPBOARD_IMAGE_RETENTION_SECS: u64 = 24 * 60 * 60;
static CLIPBOARD_IMAGE_CLEANUP_RUNNING: AtomicBool = AtomicBool::new(false);

pub struct Clipboard {
    pasteboard: id,
}

impl Clipboard {
    pub fn new() -> Self {
        let pasteboard = unsafe { NSPasteboard::generalPasteboard(nil) };
        if pasteboard.is_null() {
            panic!("NSPasteboard::generalPasteboard returned null");
        }
        Clipboard { pasteboard }
    }

    fn read_image_data(&self) -> anyhow::Result<Option<(Vec<u8>, &'static str)>> {
        unsafe {
            for (uti, extension) in [(PNG_PASTEBOARD_TYPE, "png"), (TIFF_PASTEBOARD_TYPE, "tiff")] {
                let data: id = msg_send![self.pasteboard, dataForType:*nsstring(uti)];
                if data.is_null() {
                    continue;
                }

                let len: usize = msg_send![data, length];
                if len == 0 {
                    continue;
                }
                anyhow::ensure!(
                    len <= MAX_CLIPBOARD_IMAGE_BYTES,
                    "clipboard image exceeds {} bytes",
                    MAX_CLIPBOARD_IMAGE_BYTES
                );

                let bytes: *const u8 = msg_send![data, bytes];
                anyhow::ensure!(!bytes.is_null(), "clipboard image bytes returned null");

                let data = std::slice::from_raw_parts(bytes, len).to_vec();
                return Ok(Some((data, extension)));
            }
        }

        Ok(None)
    }

    fn write_image_to_runtime_dir(
        &self,
        image_data: &[u8],
        extension: &str,
    ) -> anyhow::Result<PathBuf> {
        let dir = config::RUNTIME_DIR.join(CLIPBOARD_IMAGE_DIR);
        config::create_user_owned_dirs(&dir)?;
        // Spawn cleanup in background to avoid blocking paste operation
        if CLIPBOARD_IMAGE_CLEANUP_RUNNING
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
        {
            let dir_clone = dir.clone();
            promise::spawn::spawn(async move {
                if let Err(err) = Self::cleanup_runtime_image_dir_static(&dir_clone) {
                    log::warn!(
                        "failed to prune clipboard image cache at {}: {err:#}",
                        dir_clone.display()
                    );
                }
                CLIPBOARD_IMAGE_CLEANUP_RUNNING.store(false, Ordering::Release);
            })
            .detach();
        }

        let pid = std::process::id();
        for attempt in 0..64u32 {
            let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
            let file_name =
                format!("{CLIPBOARD_IMAGE_FILE_PREFIX}{pid}-{now}-{attempt}.{extension}");
            let path = dir.join(file_name);

            let mut options = std::fs::OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            options.mode(0o600);

            match options.open(&path) {
                Ok(mut file) => {
                    use std::io::Write;
                    file.write_all(image_data)?;
                    return Ok(path);
                }
                Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(err) => return Err(err.into()),
            }
        }

        anyhow::bail!("failed to allocate unique clipboard image path")
    }

    fn cleanup_runtime_image_dir_static(dir: &Path) -> anyhow::Result<()> {
        let retention = Duration::from_secs(CLIPBOARD_IMAGE_RETENTION_SECS);
        let now = SystemTime::now();
        let mut retained = Vec::new();

        for entry in std::fs::read_dir(dir)? {
            let entry = match entry {
                Ok(entry) => entry,
                Err(err) => {
                    log::warn!(
                        "failed to list clipboard image cache entry in {}: {err:#}",
                        dir.display()
                    );
                    continue;
                }
            };

            let path = entry.path();
            if !path.is_file() {
                continue;
            }

            let Some(file_name) = path.file_name().and_then(|name| name.to_str()) else {
                continue;
            };
            if !file_name.starts_with(CLIPBOARD_IMAGE_FILE_PREFIX) {
                continue;
            }

            let metadata = match entry.metadata() {
                Ok(metadata) => metadata,
                Err(err) => {
                    log::warn!(
                        "failed to read metadata for clipboard image {}: {err:#}",
                        path.display()
                    );
                    continue;
                }
            };
            let modified = metadata.modified().unwrap_or(UNIX_EPOCH);
            let expired = now
                .duration_since(modified)
                .map(|elapsed| elapsed > retention)
                .unwrap_or(false);
            if expired {
                if let Err(err) = std::fs::remove_file(&path) {
                    if err.kind() != std::io::ErrorKind::NotFound {
                        log::warn!(
                            "failed to remove expired clipboard image {}: {err:#}",
                            path.display()
                        );
                    }
                }
                continue;
            }

            retained.push((modified, path));
        }

        if retained.len() <= MAX_CLIPBOARD_IMAGE_FILES {
            return Ok(());
        }

        retained.sort_by_key(|(modified, _)| *modified);
        let remove_count = retained.len().saturating_sub(MAX_CLIPBOARD_IMAGE_FILES);
        for (_, path) in retained.into_iter().take(remove_count) {
            if let Err(err) = std::fs::remove_file(&path) {
                if err.kind() != std::io::ErrorKind::NotFound {
                    log::warn!(
                        "failed to trim clipboard image cache file {}: {err:#}",
                        path.display()
                    );
                }
            }
        }

        Ok(())
    }

    pub fn read_data(&self) -> anyhow::Result<ClipboardData> {
        unsafe {
            let plist = self.pasteboard.propertyListForType(NSFilenamesPboardType);
            if !plist.is_null() {
                let mut filenames = vec![];
                for i in 0..plist.count() {
                    filenames.push(PathBuf::from(nsstring_to_str(plist.objectAtIndex(i))));
                }
                return Ok(ClipboardData::Files(filenames));
            }
            let s = self.pasteboard.stringForType(NSStringPboardType);
            if !s.is_null() {
                let str = nsstring_to_str(s);
                return Ok(ClipboardData::Text(str.to_string()));
            }
        }

        if let Some((image_data, extension)) = self.read_image_data()? {
            let path = self.write_image_to_runtime_dir(&image_data, extension)?;
            return Ok(ClipboardData::Image(path));
        }

        anyhow::bail!("pasteboard read returned empty");
    }

    pub fn read(&self) -> anyhow::Result<String> {
        match self.read_data()? {
            ClipboardData::Text(text) => Ok(text),
            ClipboardData::Image(_) => Ok(String::new()),
            ClipboardData::Files(paths) => {
                let quoted = paths
                    .iter()
                    .map(|path| {
                        let path_str = path.to_string_lossy().to_string();
                        match shlex::try_quote(&path_str) {
                            Ok(quoted) => quoted.into_owned(),
                            Err(err) => {
                                log::warn!(
                                    "Failed to quote path {:?} for clipboard read: {}. Using as-is.",
                                    path_str, err
                                );
                                path_str
                            }
                        }
                    })
                    .collect::<Vec<_>>()
                    .join(" ");
                Ok(quoted)
            }
        }
    }

    pub fn write(&mut self, data: String) -> anyhow::Result<()> {
        unsafe {
            self.pasteboard.clearContents();
            let success: BOOL = self
                .pasteboard
                .writeObjects(NSArray::arrayWithObject(nil, *nsstring(&data)));
            anyhow::ensure!(success == YES, "pasteboard write returned false");
            Ok(())
        }
    }
}
