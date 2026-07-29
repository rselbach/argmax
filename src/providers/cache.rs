//! Bounded session-local caching for successful dynamic provider results.

use std::error::Error;
use std::ffi::OsStr;
use std::fmt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::completion::{GeneratorKind, GeneratorSpec};

use super::{
    DynamicItem, DynamicMetadata, GitBranchScope, MAX_DYNAMIC_ITEM_BYTES, MAX_DYNAMIC_ITEMS,
};

/// Largest number of dynamic snapshots retained in one session.
pub const MAX_DYNAMIC_CACHE_ENTRIES: usize = 128;
/// Largest combined key and result payload retained by the dynamic cache.
pub const MAX_DYNAMIC_CACHE_BYTES: usize = 4 * 1024 * 1024;
/// Largest serialized logical identity retained for one cache key.
pub const MAX_DYNAMIC_CACHE_KEY_BYTES: usize = 16 * 1024;
/// Largest number of command arguments retained in one cache key.
pub const MAX_DYNAMIC_CACHE_ARGUMENTS: usize = 128;

const MAX_DYNAMIC_CACHE_KEY_FIELD_BYTES: usize = 4 * 1024;

/// Exact, bounded identity for one dynamic generator request.
#[derive(Clone, Eq, PartialEq)]
pub struct DynamicCacheKey {
    generator: GeneratorKind,
    max_results: usize,
    cache_ttl: Duration,
    cwd: PathBuf,
    arguments: Vec<String>,
    partial: String,
    payload_bytes: usize,
}

impl DynamicCacheKey {
    /// Validates and retains one generator request identity.
    ///
    /// # Errors
    ///
    /// Returns a content-free error when a field, argument count, or combined
    /// key exceeds its hard bound.
    pub fn new(
        spec: &GeneratorSpec,
        cwd: &Path,
        arguments: Vec<String>,
        partial: &str,
    ) -> Result<Self, DynamicCacheKeyError> {
        if let Err(error) = spec.validate() {
            return Err(DynamicCacheKeyError::new(
                error.field,
                "generator specification is invalid",
            ));
        }
        if !cwd.is_absolute() {
            return Err(DynamicCacheKeyError::new("cwd", "must be an absolute path"));
        }
        if arguments.len() > MAX_DYNAMIC_CACHE_ARGUMENTS {
            return Err(DynamicCacheKeyError::new(
                "arguments",
                "contains too many values",
            ));
        }

        let cwd_bytes = os_str_bytes(cwd.as_os_str());
        validate_key_field("cwd", cwd_bytes)?;
        validate_key_field("partial", partial.len())?;
        let argument_struct_bytes = arguments
            .len()
            .checked_mul(std::mem::size_of::<String>())
            .ok_or_else(|| DynamicCacheKeyError::new("key", "byte size overflowed"))?;
        let mut payload_bytes = generator_payload_bytes(&spec.kind)
            .checked_add(std::mem::size_of::<usize>())
            .and_then(|bytes| bytes.checked_add(std::mem::size_of::<Duration>()))
            .and_then(|bytes| bytes.checked_add(argument_struct_bytes))
            .and_then(|bytes| bytes.checked_add(cwd_bytes))
            .and_then(|bytes| bytes.checked_add(partial.len()))
            .ok_or_else(|| DynamicCacheKeyError::new("key", "byte size overflowed"))?;

        for argument in &arguments {
            validate_key_field("arguments", argument.len())?;
            payload_bytes = payload_bytes
                .checked_add(argument.len())
                .ok_or_else(|| DynamicCacheKeyError::new("key", "byte size overflowed"))?;
        }
        if payload_bytes > MAX_DYNAMIC_CACHE_KEY_BYTES {
            return Err(DynamicCacheKeyError::new(
                "key",
                "combined byte size exceeds the limit",
            ));
        }

        Ok(Self {
            generator: normalize_generator(spec.kind.clone()),
            max_results: spec.max_results,
            cache_ttl: spec.cache_ttl,
            cwd: PathBuf::from(cwd.as_os_str()),
            arguments: normalize_strings(arguments),
            partial: partial.to_owned(),
            payload_bytes,
        })
    }

    /// Generator declaration included in this identity.
    #[must_use]
    pub const fn generator(&self) -> &GeneratorKind {
        &self.generator
    }

    /// Result limit included in this cache identity.
    #[must_use]
    pub const fn max_results(&self) -> usize {
        self.max_results
    }

    /// Successful-result lifetime included in this cache identity.
    #[must_use]
    pub const fn cache_ttl(&self) -> Duration {
        self.cache_ttl
    }

    /// Exact working directory included in this identity.
    #[must_use]
    pub fn cwd(&self) -> &Path {
        &self.cwd
    }

    /// Number of command arguments retained without exposing their content.
    #[must_use]
    pub fn argument_count(&self) -> usize {
        self.arguments.len()
    }

    /// Retained logical key payload in bytes.
    #[must_use]
    pub const fn payload_bytes(&self) -> usize {
        self.payload_bytes
    }
}

impl fmt::Debug for DynamicCacheKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut output = formatter.debug_struct("DynamicCacheKey");
        output
            .field("generator", &generator_name(&self.generator))
            .field("max_results", &self.max_results)
            .field("cache_ttl", &self.cache_ttl);
        if let GeneratorKind::Filesystem(filesystem) = &self.generator {
            output
                .field("directory_only", &filesystem.directory_only)
                .field("extension_count", &filesystem.extensions.len())
                .field(
                    "extension_bytes",
                    &filesystem.extensions.iter().map(String::len).sum::<usize>(),
                )
                .field("max_entries", &filesystem.max_entries);
        }
        output
            .field("cwd_bytes", &os_str_bytes(self.cwd.as_os_str()))
            .field("argument_count", &self.arguments.len())
            .field("partial_bytes", &self.partial.len())
            .field("payload_bytes", &self.payload_bytes)
            .finish()
    }
}

/// Validation failure for a dynamic cache key.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DynamicCacheKeyError {
    /// Responsible logical field.
    pub field: &'static str,
    /// Content-free validation reason.
    pub reason: &'static str,
}

impl DynamicCacheKeyError {
    const fn new(field: &'static str, reason: &'static str) -> Self {
        Self { field, reason }
    }
}

impl fmt::Display for DynamicCacheKeyError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.field, self.reason)
    }
}

impl Error for DynamicCacheKeyError {}

/// Result of attempting to retain one successful dynamic snapshot.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DynamicCacheAdmission {
    /// A new key was retained.
    Inserted,
    /// An existing key was atomically replaced.
    Replaced,
    /// The validated TTL was not representable from the supplied time.
    InvalidTtl,
    /// The item count, an item field, or the entry payload exceeded a hard bound.
    Oversized,
}

struct DynamicCacheEntry {
    key: DynamicCacheKey,
    items: Vec<DynamicItem>,
    expires_at: Instant,
    payload_bytes: usize,
}

/// Bounded least-recently-used cache for successful dynamic provider output.
///
/// Failures have no insertion API and therefore cannot replace a prior success.
/// The front entry is least recently used; a successful lookup moves it to the
/// back. Diagnostics reveal only counts and byte sizes.
#[derive(Default)]
pub struct DynamicResultCache {
    entries: Vec<DynamicCacheEntry>,
    payload_bytes: usize,
}

impl DynamicResultCache {
    /// Returns a cached successful result and refreshes its LRU position.
    ///
    /// Expiry is inclusive: an entry is unavailable when `now` equals its
    /// deadline.
    pub fn get(&mut self, key: &DynamicCacheKey, now: Instant) -> Option<&[DynamicItem]> {
        let position = self.entries.iter().position(|entry| &entry.key == key)?;
        if now >= self.entries[position].expires_at {
            self.remove(position);
            return None;
        }

        let entry = self.entries.remove(position);
        self.entries.push(entry);
        self.entries.last().map(|entry| entry.items.as_slice())
    }

    /// Retains one successful result with its validated generator TTL.
    ///
    /// Rejected admission leaves an existing value for the same key untouched.
    pub fn insert_success(
        &mut self,
        key: DynamicCacheKey,
        items: Vec<DynamicItem>,
        now: Instant,
    ) -> DynamicCacheAdmission {
        let Some(expires_at) = now.checked_add(key.cache_ttl()) else {
            return DynamicCacheAdmission::InvalidTtl;
        };
        let Some(payload_bytes) = entry_payload_bytes(&key, &items) else {
            return DynamicCacheAdmission::Oversized;
        };
        let items = normalize_items(items);

        self.remove_expired(now);
        let existing = self.entries.iter().position(|entry| entry.key == key);
        if let Some(position) = existing {
            self.remove(position);
        }
        while self.entries.len() >= MAX_DYNAMIC_CACHE_ENTRIES
            || self.payload_bytes.saturating_add(payload_bytes) > MAX_DYNAMIC_CACHE_BYTES
        {
            self.remove(0);
        }

        self.payload_bytes += payload_bytes;
        self.entries.push(DynamicCacheEntry {
            key,
            items,
            expires_at,
            payload_bytes,
        });
        if existing.is_some() {
            DynamicCacheAdmission::Replaced
        } else {
            DynamicCacheAdmission::Inserted
        }
    }

    /// Invalidates every entry for an exact working directory.
    ///
    /// Returns the number removed.
    pub fn invalidate_cwd(&mut self, cwd: &Path) -> usize {
        let before = self.entries.len();
        let mut index = 0;
        while index < self.entries.len() {
            if self.entries[index].key.cwd() == cwd {
                self.remove(index);
            } else {
                index += 1;
            }
        }
        before - self.entries.len()
    }

    /// Removes every retained result.
    pub fn clear(&mut self) {
        self.entries.clear();
        self.payload_bytes = 0;
    }

    /// Number of retained successful snapshots.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether no successful snapshots are retained.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Combined retained logical key and item payload bytes.
    #[must_use]
    pub const fn payload_bytes(&self) -> usize {
        self.payload_bytes
    }

    fn remove_expired(&mut self, now: Instant) {
        let mut index = 0;
        while index < self.entries.len() {
            if now >= self.entries[index].expires_at {
                self.remove(index);
            } else {
                index += 1;
            }
        }
    }

    fn remove(&mut self, position: usize) {
        let entry = self.entries.remove(position);
        self.payload_bytes = self.payload_bytes.saturating_sub(entry.payload_bytes);
    }
}

impl fmt::Debug for DynamicResultCache {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DynamicResultCache")
            .field("entries", &self.entries.len())
            .field("payload_bytes", &self.payload_bytes)
            .finish()
    }
}

fn validate_key_field(field: &'static str, bytes: usize) -> Result<(), DynamicCacheKeyError> {
    if bytes > MAX_DYNAMIC_CACHE_KEY_FIELD_BYTES {
        return Err(DynamicCacheKeyError::new(
            field,
            "byte size exceeds the field limit",
        ));
    }
    Ok(())
}

fn generator_payload_bytes(generator: &GeneratorKind) -> usize {
    let base = std::mem::size_of::<GeneratorKind>();
    match generator {
        GeneratorKind::Filesystem(filesystem) => filesystem.extensions.iter().fold(
            base.saturating_add(
                filesystem
                    .extensions
                    .len()
                    .saturating_mul(std::mem::size_of::<String>()),
            ),
            |bytes, extension| bytes.saturating_add(extension.len()),
        ),
        _ => base,
    }
}

fn generator_name(generator: &GeneratorKind) -> &'static str {
    match generator {
        GeneratorKind::GitBranches => "git-branches",
        GeneratorKind::GitRemotes => "git-remotes",
        GeneratorKind::GitTags => "git-tags",
        GeneratorKind::GitStashes => "git-stashes",
        GeneratorKind::GitCommits => "git-commits",
        GeneratorKind::GitFiles => "git-files",
        GeneratorKind::PackageScripts => "package-scripts",
        GeneratorKind::MakeTargets => "make-targets",
        GeneratorKind::JustRecipes => "just-recipes",
        GeneratorKind::DockerContainers => "docker-containers",
        GeneratorKind::DockerImages => "docker-images",
        GeneratorKind::SshHosts => "ssh-hosts",
        GeneratorKind::ZoxideDirectories => "zoxide-directories",
        GeneratorKind::Packages => "packages",
        GeneratorKind::Processes => "processes",
        GeneratorKind::Services => "services",
        GeneratorKind::EnvironmentVariables => "environment-variables",
        GeneratorKind::FileTypes => "file-types",
        GeneratorKind::Filesystem(_) => "filesystem",
    }
}

fn entry_payload_bytes(key: &DynamicCacheKey, items: &[DynamicItem]) -> Option<usize> {
    if items.len() > MAX_DYNAMIC_ITEMS || items.len() > key.max_results() {
        return None;
    }
    let mut bytes = key
        .payload_bytes()
        .checked_add(std::mem::size_of::<DynamicCacheEntry>())?;
    for item in items {
        if item.value.len().saturating_add(item.description.len()) > MAX_DYNAMIC_ITEM_BYTES
            || metadata_payload_bytes(&item.metadata) > MAX_DYNAMIC_ITEM_BYTES
        {
            return None;
        }
        bytes = bytes
            .checked_add(std::mem::size_of::<DynamicItem>())?
            .checked_add(item.value.len())?
            .checked_add(item.description.len())?
            .checked_add(metadata_payload_bytes(&item.metadata))?;
        if bytes > MAX_DYNAMIC_CACHE_BYTES {
            return None;
        }
    }
    Some(bytes)
}

fn metadata_payload_bytes(metadata: &DynamicMetadata) -> usize {
    match metadata {
        DynamicMetadata::GitBranch {
            scope: GitBranchScope::Remote { remote },
            ..
        } => remote.len(),
        DynamicMetadata::Zoxide { score } => score.len(),
        DynamicMetadata::None
        | DynamicMetadata::GitBranch {
            scope: GitBranchScope::Local,
            ..
        }
        | DynamicMetadata::SshHost { .. }
        | DynamicMetadata::Process { .. } => 0,
    }
}

fn normalize_generator(mut generator: GeneratorKind) -> GeneratorKind {
    if let GeneratorKind::Filesystem(filesystem) = &mut generator {
        filesystem.extensions = normalize_strings(std::mem::take(&mut filesystem.extensions));
    }
    generator
}

fn normalize_strings(values: Vec<String>) -> Vec<String> {
    let mut normalized = values
        .into_iter()
        .map(|value| value.as_str().to_owned())
        .collect::<Vec<_>>();
    normalized.shrink_to_fit();
    normalized
}

fn normalize_items(items: Vec<DynamicItem>) -> Vec<DynamicItem> {
    let mut normalized = items
        .into_iter()
        .map(|item| DynamicItem {
            value: item.value.as_str().to_owned(),
            description: item.description.as_str().to_owned(),
            kind: item.kind,
            metadata: normalize_metadata(item.metadata),
        })
        .collect::<Vec<_>>();
    normalized.shrink_to_fit();
    normalized
}

fn normalize_metadata(metadata: DynamicMetadata) -> DynamicMetadata {
    match metadata {
        DynamicMetadata::GitBranch {
            scope: GitBranchScope::Remote { remote },
            active,
        } => DynamicMetadata::GitBranch {
            scope: GitBranchScope::Remote {
                remote: remote.as_str().to_owned(),
            },
            active,
        },
        DynamicMetadata::Zoxide { score } => DynamicMetadata::Zoxide {
            score: score.as_str().to_owned(),
        },
        other => other,
    }
}

#[cfg(unix)]
fn os_str_bytes(value: &OsStr) -> usize {
    use std::os::unix::ffi::OsStrExt as _;

    value.as_bytes().len()
}

#[cfg(not(unix))]
fn os_str_bytes(value: &OsStr) -> usize {
    value.to_string_lossy().len()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::completion::{FilesystemGenerator, GeneratorKind, GeneratorTarget};
    use crate::providers::{DynamicItemKind, DynamicMetadata};

    fn generator_spec(kind: GeneratorKind) -> GeneratorSpec {
        GeneratorSpec::new(kind, GeneratorTarget::Positional(0))
    }

    fn key(name: &str, cwd: &str) -> DynamicCacheKey {
        let spec = generator_spec(GeneratorKind::GitBranches);
        DynamicCacheKey::new(
            &spec,
            Path::new(cwd),
            vec!["git".into(), "branch".into()],
            name,
        )
        .unwrap()
    }

    fn item(value: &str) -> DynamicItem {
        DynamicItem {
            value: value.into(),
            description: "local Git branch".into(),
            kind: DynamicItemKind::GitBranch,
            metadata: DynamicMetadata::None,
        }
    }

    #[test]
    fn exact_expiry_and_successful_replacement_are_deterministic() {
        let now = Instant::now();
        let cache_key = key("m", "/tmp/Greendale");
        let mut cache = DynamicResultCache::default();

        assert_eq!(
            cache.insert_success(cache_key.clone(), vec![item("main")], now,),
            DynamicCacheAdmission::Inserted
        );
        assert_eq!(
            cache.get(&cache_key, now + Duration::from_secs(4)).unwrap()[0].value,
            "main"
        );
        assert_eq!(
            cache.insert_success(
                cache_key.clone(),
                vec![item("maintenance")],
                now + Duration::from_secs(4),
            ),
            DynamicCacheAdmission::Replaced
        );
        assert_eq!(
            cache.get(&cache_key, now + Duration::from_secs(8)).unwrap()[0].value,
            "maintenance"
        );
        assert!(
            cache
                .get(&cache_key, now + Duration::from_secs(9))
                .is_none()
        );
        assert!(cache.is_empty());
        assert_eq!(cache.payload_bytes(), 0);
    }

    #[test]
    fn lru_access_preserves_the_touched_entry_at_capacity() {
        let now = Instant::now();
        let mut cache = DynamicResultCache::default();
        for index in 0..MAX_DYNAMIC_CACHE_ENTRIES {
            assert_eq!(
                cache.insert_success(
                    key(&format!("branch-{index}"), "/tmp/Greendale"),
                    vec![item("main")],
                    now,
                ),
                DynamicCacheAdmission::Inserted
            );
        }
        let first = key("branch-0", "/tmp/Greendale");
        let second = key("branch-1", "/tmp/Greendale");
        assert!(cache.get(&first, now).is_some());
        cache.insert_success(key("new", "/tmp/Greendale"), vec![item("new")], now);

        assert!(cache.get(&first, now).is_some());
        assert!(cache.get(&second, now).is_none());
        assert_eq!(cache.len(), MAX_DYNAMIC_CACHE_ENTRIES);
    }

    #[test]
    fn oversized_replacement_keeps_prior_success() {
        let now = Instant::now();
        let cache_key = key("m", "/tmp/Greendale");
        let mut cache = DynamicResultCache::default();
        cache.insert_success(cache_key.clone(), vec![item("main")], now);
        let oversized = item(&"x".repeat(MAX_DYNAMIC_ITEM_BYTES + 1));

        assert_eq!(
            cache.insert_success(cache_key.clone(), vec![oversized], now,),
            DynamicCacheAdmission::Oversized
        );
        assert_eq!(cache.get(&cache_key, now).unwrap()[0].value, "main");

        let oversized_metadata = DynamicItem {
            value: "main".into(),
            description: String::new(),
            kind: DynamicItemKind::ZoxideDirectory,
            metadata: DynamicMetadata::Zoxide {
                score: "1".repeat(MAX_DYNAMIC_ITEM_BYTES + 1),
            },
        };
        assert_eq!(
            cache.insert_success(cache_key.clone(), vec![oversized_metadata], now,),
            DynamicCacheAdmission::Oversized
        );
        assert_eq!(cache.get(&cache_key, now).unwrap()[0].value, "main");
    }

    #[test]
    fn key_bounds_and_debug_output_do_not_expose_query_content() {
        let secret = "Troy-secret-token";
        let mut argument = String::with_capacity(1024 * 1024);
        argument.push_str(secret);
        let mut partial = String::with_capacity(1024 * 1024);
        partial.push_str(secret);
        let secret_cwd = PathBuf::from(format!("/tmp/{secret}"));
        let spec = generator_spec(GeneratorKind::Filesystem(FilesystemGenerator {
            extensions: vec![secret.to_owned()],
            ..FilesystemGenerator::default()
        }));
        let key = DynamicCacheKey::new(&spec, &secret_cwd, vec![argument], &partial).unwrap();
        let debug = format!("{key:?}");
        assert!(!debug.contains(secret));
        assert_eq!(key.argument_count(), 1);
        assert!(key.payload_bytes() < MAX_DYNAMIC_CACHE_KEY_BYTES);
        assert!(key.arguments[0].capacity() < 1024);
        assert!(key.partial.capacity() < 1024);

        let too_many = vec![String::new(); MAX_DYNAMIC_CACHE_ARGUMENTS + 1];
        let spec = generator_spec(GeneratorKind::GitCommits);
        assert_eq!(
            DynamicCacheKey::new(&spec, Path::new("/tmp"), too_many, "",)
                .unwrap_err()
                .field,
            "arguments"
        );
        let oversized = "x".repeat(MAX_DYNAMIC_CACHE_KEY_FIELD_BYTES + 1);
        assert_eq!(
            DynamicCacheKey::new(&spec, Path::new("/tmp"), Vec::new(), &oversized,)
                .unwrap_err()
                .field,
            "partial"
        );
    }

    #[test]
    fn cwd_invalidation_is_exact_and_cache_debug_is_redacted() {
        let now = Instant::now();
        let mut cache = DynamicResultCache::default();
        for cwd in ["/tmp/Greendale", "/tmp/CityCollege"] {
            cache.insert_success(key("m", cwd), vec![item("main")], now);
        }
        assert_eq!(cache.invalidate_cwd(Path::new("/tmp/Greendale")), 1);
        assert_eq!(cache.len(), 1);
        let debug = format!("{cache:?}");
        assert_eq!(
            debug,
            format!(
                "DynamicResultCache {{ entries: 1, payload_bytes: {} }}",
                cache.payload_bytes()
            )
        );
        assert!(!debug.contains("main"));
        cache.clear();
        assert!(cache.is_empty());
    }

    #[test]
    fn retained_item_strings_are_normalized_without_spare_capacity() {
        let now = Instant::now();
        let cache_key = key("m", "/tmp/Greendale");
        let mut value = String::with_capacity(1024 * 1024);
        value.push_str("main");
        let mut cache = DynamicResultCache::default();
        cache.insert_success(
            cache_key.clone(),
            vec![DynamicItem {
                value,
                description: "branch".into(),
                kind: DynamicItemKind::GitBranch,
                metadata: DynamicMetadata::GitBranch {
                    scope: GitBranchScope::Remote {
                        remote: String::with_capacity(1024 * 1024),
                    },
                    active: false,
                },
            }],
            now,
        );

        let cached = &cache.get(&cache_key, now).unwrap()[0];
        assert!(cached.value.capacity() < 1024);
        let DynamicMetadata::GitBranch {
            scope: GitBranchScope::Remote { remote },
            ..
        } = &cached.metadata
        else {
            panic!("expected remote metadata");
        };
        assert!(remote.capacity() < 1024);
    }

    #[test]
    fn key_identity_includes_generator_policy_arguments_partial_and_cwd() {
        let baseline = key("m", "/tmp/Greendale");
        let different_cwd = key("m", "/tmp/CityCollege");
        let different_partial = key("ma", "/tmp/Greendale");
        let different_generator_spec = generator_spec(GeneratorKind::GitTags);
        let different_generator = DynamicCacheKey::new(
            &different_generator_spec,
            Path::new("/tmp/Greendale"),
            vec!["git".into(), "branch".into()],
            "m",
        )
        .unwrap();
        let baseline_spec = generator_spec(GeneratorKind::GitBranches);
        let different_arguments = DynamicCacheKey::new(
            &baseline_spec,
            Path::new("/tmp/Greendale"),
            vec!["git".into(), "tag".into()],
            "m",
        )
        .unwrap();

        assert_ne!(baseline, different_cwd);
        assert_ne!(baseline, different_partial);
        assert_ne!(baseline, different_generator);
        assert_ne!(baseline, different_arguments);

        let mut lower_limit_spec = baseline_spec.clone();
        lower_limit_spec.max_results -= 1;
        let lower_limit = DynamicCacheKey::new(
            &lower_limit_spec,
            Path::new("/tmp/Greendale"),
            vec!["git".into(), "branch".into()],
            "m",
        )
        .unwrap();
        let mut shorter_ttl_spec = baseline_spec;
        shorter_ttl_spec.cache_ttl = Duration::from_secs(1);
        let shorter_ttl = DynamicCacheKey::new(
            &shorter_ttl_spec,
            Path::new("/tmp/Greendale"),
            vec!["git".into(), "branch".into()],
            "m",
        )
        .unwrap();
        assert_ne!(baseline, lower_limit);
        assert_ne!(baseline, shorter_ttl);
    }

    #[test]
    fn invalid_generator_relative_cwd_and_policy_overflow_fail_closed() {
        let spec = generator_spec(GeneratorKind::GitBranches);
        assert_eq!(
            DynamicCacheKey::new(&spec, Path::new("relative"), Vec::new(), "")
                .unwrap_err()
                .field,
            "cwd"
        );

        let invalid_spec = generator_spec(GeneratorKind::Filesystem(FilesystemGenerator {
            extensions: vec!["rs".to_owned(); 33],
            ..FilesystemGenerator::default()
        }));
        assert_eq!(
            DynamicCacheKey::new(&invalid_spec, Path::new("/tmp"), Vec::new(), "")
                .unwrap_err()
                .field,
            "kind.filesystem.extensions"
        );

        let mut one_result_spec = spec;
        one_result_spec.max_results = 1;
        let limited_key = DynamicCacheKey::new(
            &one_result_spec,
            Path::new("/tmp/Greendale"),
            vec!["git".into(), "branch".into()],
            "m",
        )
        .unwrap();
        let now = Instant::now();
        let mut cache = DynamicResultCache::default();
        assert_eq!(
            cache.insert_success(limited_key.clone(), vec![item("main")], now),
            DynamicCacheAdmission::Inserted
        );
        assert_eq!(
            cache.insert_success(
                limited_key.clone(),
                vec![item("main"), item("maintenance")],
                now,
            ),
            DynamicCacheAdmission::Oversized
        );
        assert_eq!(cache.get(&limited_key, now).unwrap()[0].value, "main");
    }
}
