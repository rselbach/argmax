//! Lazy persistent-history caching independent of shell storage syntax.

use std::collections::VecDeque;
use std::fs;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use super::{HistoryEntry, merge_history};

/// Default maximum number of history-file bytes read on one cache fill.
pub const DEFAULT_MAX_HISTORY_BYTES: usize = 8 * 1024 * 1024;

/// Default maximum number of commands retained from the current session.
pub const DEFAULT_MAX_SESSION_ENTRIES: usize = 16 * 1024;

/// Filesystem identity used to detect changes to the active history file.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HistoryFileKey {
    path: PathBuf,
    length: Option<u64>,
    modified: Option<SystemTime>,
    identity: Option<(u64, u64)>,
}

impl HistoryFileKey {
    /// Captures the path and all currently available change metadata.
    #[must_use]
    pub fn capture(path: impl AsRef<Path>) -> Self {
        let path = path.as_ref();
        let metadata = fs::metadata(path).ok();
        #[cfg(unix)]
        let identity = metadata.as_ref().map(file_identity);
        #[cfg(not(unix))]
        let identity = None;
        Self {
            path: path.to_path_buf(),
            length: metadata.as_ref().map(fs::Metadata::len),
            modified: metadata
                .as_ref()
                .and_then(|metadata| metadata.modified().ok()),
            identity,
        }
    }

    /// Active history path represented by this key.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// File length when metadata was available.
    #[must_use]
    pub const fn length(&self) -> Option<u64> {
        self.length
    }

    /// Last-modified time when the filesystem supplied one.
    #[must_use]
    pub const fn modified(&self) -> Option<SystemTime> {
        self.modified
    }
}

#[cfg(unix)]
fn file_identity(metadata: &fs::Metadata) -> (u64, u64) {
    use std::os::unix::fs::MetadataExt as _;

    (metadata.dev(), metadata.ino())
}

#[derive(Clone, Debug)]
struct CachedHistory {
    key: HistoryFileKey,
    entries: Vec<HistoryEntry>,
}

/// Lazy cache for one active persistent history path and live session entries.
///
/// File syntax remains outside this type: callers provide a parser on each
/// request, and it is invoked only when the active path has not been loaded or
/// its available length/modified metadata changed. File failures become an
/// empty persistent snapshot so they cannot affect unrelated completion.
#[derive(Clone, Debug)]
pub struct HistoryCache {
    cached: Option<CachedHistory>,
    session: VecDeque<HistoryEntry>,
    max_history_bytes: usize,
    max_session_entries: usize,
}

impl HistoryCache {
    /// Creates an empty cache with explicit file and session bounds.
    #[must_use]
    pub fn with_limits(max_history_bytes: usize, max_session_entries: usize) -> Self {
        Self {
            cached: None,
            session: VecDeque::new(),
            max_history_bytes,
            max_session_entries,
        }
    }

    /// Records one command executed in the current shell session immediately.
    ///
    /// Empty commands are ignored. Once the configured bound is reached, the
    /// oldest session command is discarded.
    pub fn record_session(&mut self, entry: HistoryEntry) {
        if entry.command.trim().is_empty() || self.max_session_entries == 0 {
            return;
        }
        if self.session.len() == self.max_session_entries {
            self.session.pop_front();
        }
        self.session.push_back(entry);
    }

    /// Returns persistent and current-session history in newest-first order.
    ///
    /// `parser` receives at most the configured number of raw bytes and should
    /// return valid records in file order, oldest to newest. Bytes are passed
    /// undecoded because a format may escape bytes that are not valid UTF-8 on
    /// their own, so only the parser knows how to recover the original text.
    /// Missing, unreadable, and partially malformed files are isolated to this
    /// cache, with record recovery left to the parser. Exact duplicate commands
    /// keep their newest occurrence.
    #[must_use]
    pub fn merged<F>(&mut self, history_path: impl AsRef<Path>, parser: F) -> Vec<HistoryEntry>
    where
        F: FnOnce(&[u8]) -> Vec<HistoryEntry>,
    {
        let key = HistoryFileKey::capture(history_path);
        if self.cached.as_ref().is_none_or(|cached| cached.key != key) {
            let entries = read_history_tail(key.path(), self.max_history_bytes)
                .map_or_else(|_| Vec::new(), |contents| parser(&contents));
            self.cached = Some(CachedHistory { key, entries });
        }

        let persistent = self
            .cached
            .as_ref()
            .map_or(&[][..], |cached| cached.entries.as_slice());
        merge_history(persistent, self.session.make_contiguous())
    }

    /// Forgets the persistent snapshot so the next request reloads it.
    pub fn invalidate(&mut self) {
        self.cached = None;
    }

    /// Removes commands recorded during the current session.
    pub fn clear_session(&mut self) {
        self.session.clear();
    }

    /// Returns the currently cached filesystem key, if persistent history loaded.
    #[must_use]
    pub fn cached_key(&self) -> Option<&HistoryFileKey> {
        self.cached.as_ref().map(|cached| &cached.key)
    }
}

impl Default for HistoryCache {
    fn default() -> Self {
        Self::with_limits(DEFAULT_MAX_HISTORY_BYTES, DEFAULT_MAX_SESSION_ENTRIES)
    }
}

fn read_history_tail(path: &Path, max_bytes: usize) -> io::Result<Vec<u8>> {
    if max_bytes == 0 {
        return Ok(Vec::new());
    }

    let mut file = fs::File::open(path)?;
    let length = file.seek(SeekFrom::End(0))?;
    let max_bytes_u64 = u64::try_from(max_bytes).unwrap_or(u64::MAX);
    let start = length.saturating_sub(max_bytes_u64);
    file.seek(SeekFrom::Start(start))?;

    let capacity = usize::try_from(length.saturating_sub(start))
        .unwrap_or(max_bytes)
        .min(max_bytes);
    let mut bytes = Vec::with_capacity(capacity);
    file.take(max_bytes_u64).read_to_end(&mut bytes)?;

    if start == 0 {
        return Ok(bytes);
    }
    Ok(bytes
        .iter()
        .position(|byte| *byte == b'\n')
        .map_or_else(Vec::new, |newline| bytes[newline + 1..].to_vec()))
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::*;

    static TEMP_ID: AtomicU64 = AtomicU64::new(0);

    struct TempDirectory(PathBuf);

    impl TempDirectory {
        fn new() -> Self {
            let identifier = TEMP_ID.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "argmax-history-cache-test-{}-{identifier}",
                std::process::id()
            ));
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }

        fn path(&self, name: &str) -> PathBuf {
            self.0.join(name)
        }
    }

    impl Drop for TempDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn loads_lazily_once_and_invalidates_when_metadata_changes() {
        let temp = TempDirectory::new();
        let path = temp.path("bash_history");
        fs::write(&path, "cargo test --package greendale\n").unwrap();
        let calls = Cell::new(0);
        let mut cache = HistoryCache::default();
        assert_eq!(calls.get(), 0);

        let first = cache.merged(&path, |contents| {
            calls.set(calls.get() + 1);
            line_parser(contents)
        });
        let second = cache.merged(&path, |contents| {
            calls.set(calls.get() + 1);
            line_parser(contents)
        });

        assert_eq!(calls.get(), 1);
        assert_eq!(first, second);
        assert_eq!(commands(&first), ["cargo test --package greendale"]);
        assert!(cache.cached_key().unwrap().modified().is_some());

        fs::write(
            &path,
            "cargo test --package greendale\ngit log --author='Troy Barnes'\n",
        )
        .unwrap();
        let changed = cache.merged(&path, |contents| {
            calls.set(calls.get() + 1);
            line_parser(contents)
        });

        assert_eq!(calls.get(), 2);
        assert_eq!(
            commands(&changed),
            [
                "git log --author='Troy Barnes'",
                "cargo test --package greendale"
            ]
        );
    }

    #[test]
    fn session_commands_merge_immediately_and_newest_exact_entry_wins() {
        let temp = TempDirectory::new();
        let path = temp.path("history");
        fs::write(
            &path,
            "git commit -m 'Troy joins study group'\ncargo test --package greendale\n",
        )
        .unwrap();
        let mut cache = HistoryCache::default();
        cache.record_session(
            HistoryEntry::new("cargo test --package greendale").with_timestamp(100),
        );
        cache.record_session(HistoryEntry::new("rg Annie greendale"));
        cache.record_session(
            HistoryEntry::new("cargo test --package greendale").with_timestamp(200),
        );

        let entries = cache.merged(&path, line_parser);

        assert_eq!(
            commands(&entries),
            [
                "cargo test --package greendale",
                "rg Annie greendale",
                "git commit -m 'Troy joins study group'",
            ]
        );
        assert_eq!(entries[0].timestamp, Some(200));
    }

    #[test]
    fn missing_and_unreadable_paths_are_cached_as_isolated_empty_snapshots() {
        let temp = TempDirectory::new();
        let missing = temp.path("missing-history");
        let calls = Cell::new(0);
        let mut cache = HistoryCache::default();
        cache.record_session(HistoryEntry::new("echo 'Cool. Cool cool cool.'"));

        for _ in 0..2 {
            let entries = cache.merged(&missing, |_| {
                calls.set(calls.get() + 1);
                vec![HistoryEntry::new("must not be parsed")]
            });
            assert_eq!(commands(&entries), ["echo 'Cool. Cool cool cool.'"]);
        }
        assert_eq!(calls.get(), 0);
        assert_eq!(cache.cached_key().unwrap().length(), None);

        let unreadable = temp.path("directory-not-file");
        fs::create_dir(&unreadable).unwrap();
        let entries = cache.merged(&unreadable, |_| {
            calls.set(calls.get() + 1);
            Vec::new()
        });
        assert_eq!(commands(&entries), ["echo 'Cool. Cool cool cool.'"]);
        assert_eq!(calls.get(), 0);
    }

    #[test]
    fn a_newly_created_missing_file_changes_the_key_and_loads() {
        let temp = TempDirectory::new();
        let path = temp.path("created-later");
        let mut cache = HistoryCache::default();
        assert!(cache.merged(&path, line_parser).is_empty());

        fs::write(&path, "echo 'Six seasons and a movie'\n").unwrap();
        let entries = cache.merged(&path, line_parser);

        assert_eq!(commands(&entries), ["echo 'Six seasons and a movie'"]);
        assert_eq!(cache.cached_key().unwrap().length(), Some(31));
    }

    #[test]
    fn parser_isolates_malformed_records_and_lossy_input() {
        let temp = TempDirectory::new();
        let path = temp.path("partial-history");
        fs::write(
            &path,
            b"cargo test --package greendale\nBROKEN\xffRECORD\ngit status\n",
        )
        .unwrap();
        let mut cache = HistoryCache::default();

        let entries = cache.merged(&path, |contents| {
            String::from_utf8_lossy(contents)
                .lines()
                .filter(|line| !line.contains("BROKEN"))
                .map(HistoryEntry::new)
                .collect()
        });

        assert_eq!(
            commands(&entries),
            ["git status", "cargo test --package greendale"]
        );
    }

    #[test]
    fn reads_only_a_bounded_tail_and_drops_its_partial_first_record() {
        let temp = TempDirectory::new();
        let path = temp.path("large-history");
        fs::write(
            &path,
            concat!(
                "echo this oldest command is outside the read bound\n",
                "git status\n",
                "cargo test --package greendale\n",
            ),
        )
        .unwrap();
        let observed_bytes = Cell::new(0);
        let mut cache = HistoryCache::with_limits(36, 100);

        let entries = cache.merged(&path, |contents| {
            observed_bytes.set(contents.len());
            line_parser(contents)
        });

        assert!(observed_bytes.get() <= 36);
        assert_eq!(commands(&entries), ["cargo test --package greendale"]);
    }

    #[test]
    fn bounds_session_storage_and_supports_explicit_invalidation() {
        let temp = TempDirectory::new();
        let path = temp.path("history");
        fs::write(&path, "git status\n").unwrap();
        let calls = Cell::new(0);
        let mut cache = HistoryCache::with_limits(1024, 2);
        cache.record_session(HistoryEntry::new("echo Troy"));
        cache.record_session(HistoryEntry::new("echo Abed"));
        cache.record_session(HistoryEntry::new("echo Annie"));
        cache.record_session(HistoryEntry::new("   "));

        let entries = cache.merged(&path, |contents| {
            calls.set(calls.get() + 1);
            line_parser(contents)
        });
        assert_eq!(
            commands(&entries),
            ["echo Annie", "echo Abed", "git status"]
        );

        cache.invalidate();
        cache.clear_session();
        let entries = cache.merged(&path, |contents| {
            calls.set(calls.get() + 1);
            line_parser(contents)
        });
        assert_eq!(calls.get(), 2);
        assert_eq!(commands(&entries), ["git status"]);
    }

    fn line_parser(contents: &[u8]) -> Vec<HistoryEntry> {
        String::from_utf8_lossy(contents)
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(HistoryEntry::new)
            .collect()
    }

    fn commands(entries: &[HistoryEntry]) -> Vec<&str> {
        entries.iter().map(|entry| entry.command.as_str()).collect()
    }
}
