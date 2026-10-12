//! Local-file resume positions. One locked, atomically replaced state file.
use crate::{Result, ffi, presentation::format_position, source::MediaSource};
use std::collections::BTreeMap;
use std::env;
use std::ffi::{CString, OsStr};
use std::fs::{self, DirBuilder, File, OpenOptions};
use std::io::{self, Write};
use std::os::unix::{
    ffi::OsStrExt,
    fs::{DirBuilderExt, MetadataExt, OpenOptionsExt},
};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

const HEADER: &str = "undefined-player-resume-v1";
static TEMP_ID: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Debug, PartialEq)]
struct Entry {
    seconds: f64,
    size: u64,
    modified: i64,
    modified_ns: i64,
}

impl Entry {
    fn same_file(&self, other: &Self) -> bool {
        self.size == other.size
            && self.modified == other.modified
            && self.modified_ns == other.modified_ns
    }
}

fn state_path(state: Option<&OsStr>, home: Option<&OsStr>) -> Option<PathBuf> {
    let base = state
        .map(Path::new)
        .filter(|path| path.is_absolute())
        .map(Path::to_owned)
        .or_else(|| {
            home.map(Path::new)
                .filter(|path| path.is_absolute())
                .map(|path| path.join(".local/state"))
        })?;
    Some(base.join("undefined-player/resume"))
}

fn path_hash(path: &Path) -> Result<String> {
    let bytes = path.as_os_str().as_bytes();
    let mut hash = [0_u8; 32];
    if unsafe { ffi::up_av_sha256(bytes.as_ptr(), bytes.len(), hash.as_mut_ptr()) } == 0 {
        return Err("could not hash media path".into());
    }
    Ok(hash.iter().map(|byte| format!("{byte:02x}")).collect())
}

fn read_entries(path: &Path) -> Result<BTreeMap<String, Entry>> {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(BTreeMap::new()),
        Err(error) => return Err(error.to_string()),
    };
    let mut lines = text.lines();
    if lines.next() != Some(HEADER) {
        return Err("unrecognized resume file format".into());
    }
    let mut entries = BTreeMap::new();
    for line in lines {
        let fields: Vec<_> = line.split_whitespace().collect();
        if fields.len() != 5
            || fields[0].len() != 64
            || !fields[0].bytes().all(|b| b.is_ascii_hexdigit())
        {
            continue;
        }
        let (Ok(seconds), Ok(size), Ok(modified), Ok(modified_ns)) = (
            fields[1].parse::<f64>(),
            fields[2].parse(),
            fields[3].parse(),
            fields[4].parse(),
        ) else {
            continue;
        };
        if seconds.is_finite() && seconds > 0.0 {
            entries.insert(
                fields[0].to_owned(),
                Entry {
                    seconds,
                    size,
                    modified,
                    modified_ns,
                },
            );
        }
    }
    Ok(entries)
}

struct Store {
    path: PathBuf,
    media: PathBuf,
    key: String,
    identity: Entry,
}

impl Store {
    fn new(path: PathBuf, media: &Path) -> Result<Self> {
        let media = media.canonicalize().map_err(|e| e.to_string())?;
        let metadata = media.metadata().map_err(|e| e.to_string())?;
        let key = path_hash(&media)?;
        Ok(Self {
            path,
            media,
            key,
            identity: Entry {
                seconds: 0.0,
                size: metadata.len(),
                modified: metadata.mtime(),
                modified_ns: metadata.mtime_nsec(),
            },
        })
    }

    fn load(&self) -> Result<Option<f64>> {
        Ok(read_entries(&self.path)?
            .get(&self.key)
            .filter(|entry| entry.same_file(&self.identity))
            .map(|entry| entry.seconds))
    }

    fn update(&self, position: Option<f64>) -> Result<()> {
        let directory = self.path.parent().ok_or("resume path has no parent")?;
        DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(directory)
            .map_err(|e| e.to_string())?;
        // Lock a separate inode: the data file itself is replaced by rename.
        let lock = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .open(directory.join("resume.lock"))
            .map_err(|e| e.to_string())?;
        lock.lock().map_err(|e| e.to_string())?;
        let mut entries = read_entries(&self.path)?;
        if let Some(seconds) = position.filter(|seconds| seconds.is_finite() && *seconds > 0.0) {
            // Check inside the transaction, after any newer writer has finished.
            let metadata = self.media.metadata().map_err(|e| e.to_string())?;
            if metadata.len() != self.identity.size
                || metadata.mtime() != self.identity.modified
                || metadata.mtime_nsec() != self.identity.modified_ns
            {
                return Ok(());
            }
            entries.insert(
                self.key.clone(),
                Entry {
                    seconds,
                    ..self.identity.clone()
                },
            );
        } else {
            // An older session must not clear a replacement video's entry.
            if entries
                .get(&self.key)
                .is_none_or(|entry| !entry.same_file(&self.identity))
            {
                return Ok(());
            }
            entries.remove(&self.key);
        }
        let temporary = directory.join(format!(
            ".resume-{}-{}.tmp",
            std::process::id(),
            TEMP_ID.fetch_add(1, Ordering::Relaxed)
        ));
        let result = (|| -> io::Result<()> {
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&temporary)?;
            writeln!(file, "{HEADER}")?;
            for (key, entry) in entries {
                writeln!(
                    file,
                    "{key}\t{}\t{}\t{}\t{}",
                    entry.seconds, entry.size, entry.modified, entry.modified_ns
                )?;
            }
            file.sync_all()?;
            fs::rename(&temporary, &self.path)?;
            File::open(directory)?.sync_all()
        })();
        if result.is_err() {
            let _ = fs::remove_file(&temporary);
        }
        result.map_err(|e| e.to_string())
    }
}

pub(crate) struct Session {
    store: Option<Store>,
    pub(crate) saved: Option<f64>,
    started: bool,
    finished: bool,
    position: f64,
}

impl Session {
    pub(crate) fn disabled() -> Self {
        Self {
            store: None,
            saved: None,
            started: false,
            finished: false,
            position: 0.0,
        }
    }

    pub(crate) fn open(source: &MediaSource, duration: Option<f64>) -> Result<Self> {
        let Some(media) = source.local_path() else {
            return Ok(Self::disabled());
        };
        let Some(duration) = duration.filter(|d| d.is_finite() && *d > 0.0) else {
            return Ok(Self::disabled());
        };
        let path = state_path(
            env::var_os("XDG_STATE_HOME").as_deref(),
            env::var_os("HOME").as_deref(),
        )
        .ok_or("no state directory is available")?;
        let store = Store::new(path, media)?;
        let saved = store.load()?.filter(|position| *position < duration);
        Ok(Self {
            store: Some(store),
            saved,
            started: saved.is_none(),
            finished: false,
            position: 0.0,
        })
    }

    pub(crate) fn choose(&mut self, resume: bool) {
        if !resume {
            self.remove();
        }
        self.position = if resume {
            self.saved.unwrap_or(0.0)
        } else {
            0.0
        };
        self.started = true;
    }

    pub(crate) fn observe(&mut self, position: f64) {
        if self.started && position.is_finite() {
            self.position = position.max(0.0);
            self.finished = false;
        }
    }

    fn remove(&self) {
        if let Some(store) = &self.store
            && let Err(error) = store.update(None)
        {
            eprintln!("warning: could not clear resume position: {error}");
        }
    }

    pub(crate) fn complete(&mut self) {
        if self.started {
            self.remove();
            self.finished = true;
        }
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        if self.started
            && !self.finished
            && let Some(store) = &self.store
            && let Err(error) = store.update(Some(self.position))
        {
            eprintln!("warning: could not save resume position: {error}");
        }
    }
}

pub(crate) struct Prompt {
    pub(crate) position: f64,
}

impl Prompt {
    pub(crate) fn new(position: f64) -> Self {
        Self { position }
    }

    pub(crate) fn key(&mut self, key: u32) -> Option<bool> {
        match key {
            ffi::UpKey_UP_KEY_Y => Some(true),
            ffi::UpKey_UP_KEY_N => Some(false),
            _ => None,
        }
    }

    pub(crate) fn text(&self) -> CString {
        let question = format!("Resume from {}?", format_position(self.position));
        let indent = " ".repeat(question.len().saturating_sub(5) / 2);
        CString::new(format!("{question}\n{indent}Y / N")).unwrap()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fixture(PathBuf);

    impl Fixture {
        fn new() -> Self {
            let path = env::temp_dir().join(format!(
                "undefined-player-resume-test-{}-{}",
                std::process::id(),
                TEMP_ID.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&path).unwrap();
            Self(path)
        }

        fn store(&self, name: &str) -> Store {
            let media = self.0.join(name);
            if !media.exists() {
                fs::write(&media, b"original video").unwrap();
            }
            Store::new(self.0.join("state/resume"), &media).unwrap()
        }

        fn session(&self, name: &str) -> Session {
            let store = self.store(name);
            let saved = store.load().unwrap();
            Session {
                store: Some(store),
                saved,
                started: saved.is_none(),
                finished: false,
                position: 0.0,
            }
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn old_session_cannot_delete_or_overwrite_replacement_position() {
        let fixture = Fixture::new();
        let mut old = fixture.session("video");
        old.observe(20.0);
        let replacement = fixture.0.join("replacement");
        fs::write(&replacement, b"replacement video with a different size").unwrap();
        fs::rename(&replacement, fixture.0.join("video")).unwrap();
        let current = fixture.store("video");
        current.update(Some(42.0)).unwrap();
        old.store.as_ref().unwrap().update(Some(30.0)).unwrap();
        assert_eq!(current.load().unwrap(), Some(42.0));
        old.complete();
        drop(old);
        assert_eq!(current.load().unwrap(), Some(42.0));
        current.update(None).unwrap();
        assert_eq!(current.load().unwrap(), None);
    }

    #[test]
    fn cancel_restart_resume_and_completion_preserve_expected_position() {
        let fixture = Fixture::new();
        let store = fixture.store("video");
        store.update(Some(42.0)).unwrap();
        {
            let mut session = fixture.session("video");
            session.observe(0.0); // Waiting for an answer must not update state.
        }
        assert_eq!(store.load().unwrap(), Some(42.0));
        {
            let mut session = fixture.session("video");
            session.choose(false);
            assert_eq!(store.load().unwrap(), None);
            session.observe(5.0);
        }
        assert_eq!(store.load().unwrap(), Some(5.0));
        {
            let mut session = fixture.session("video");
            session.choose(true);
            session.observe(12.0);
        }
        assert_eq!(store.load().unwrap(), Some(12.0));
        {
            let mut session = fixture.session("video");
            session.choose(true);
            session.complete();
            assert_eq!(store.load().unwrap(), None);
        }
        assert_eq!(store.load().unwrap(), None);
    }

    #[test]
    fn concurrent_updates_keep_other_videos() {
        let fixture = Fixture::new();
        let stores = (0..8)
            .map(|i| fixture.store(&format!("video-{i}")))
            .collect::<Vec<_>>();
        std::thread::scope(|scope| {
            for (i, store) in stores.iter().enumerate() {
                scope.spawn(move || store.update(Some(i as f64 + 1.0)).unwrap());
            }
        });
        stores[0].update(None).unwrap();
        assert_eq!(stores[0].load().unwrap(), None);
        for (i, store) in stores.iter().enumerate().skip(1) {
            assert_eq!(store.load().unwrap(), Some(i as f64 + 1.0));
        }
    }

    #[test]
    fn resume_prompt_uses_explicit_yes_or_no() {
        let mut prompt = Prompt::new(2846.0);
        assert_eq!(prompt.key(ffi::UpKey_UP_KEY_Y), Some(true));
        assert_eq!(prompt.key(ffi::UpKey_UP_KEY_N), Some(false));
        assert_eq!(prompt.key(ffi::UpKey_UP_KEY_RETURN), None);
        assert!(
            prompt
                .text()
                .to_str()
                .unwrap()
                .starts_with("Resume from 47:26?\n")
        );
    }
}
