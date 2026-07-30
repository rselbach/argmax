//! Declarative metadata for bounded dynamic completion generators.
//!
//! This module describes which local values a command specification requests.
//! It deliberately contains no provider or subprocess implementation.

use std::collections::BTreeSet;
use std::error::Error;
use std::fmt;
use std::time::Duration;

/// Default wall-clock budget for one dynamic generator.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_millis(150);
/// Longest wall-clock budget accepted for one dynamic generator.
pub const MAX_TIMEOUT: Duration = Duration::from_secs(1);
/// Default lifetime for a successful dynamic result cache entry.
pub const DEFAULT_CACHE_TTL: Duration = Duration::from_secs(5);
/// Longest cache lifetime accepted for dynamic results.
pub const MAX_CACHE_TTL: Duration = Duration::from_secs(300);
/// Default maximum number of values returned by one generator.
pub const DEFAULT_MAX_RESULTS: usize = 100;
/// Largest result set accepted from one generator.
pub const MAX_RESULTS: usize = 500;
/// Default maximum number of entries inspected by a filesystem generator.
pub const DEFAULT_FILESYSTEM_SCAN_LIMIT: usize = 4_096;
/// Largest directory scan accepted from a filesystem generator.
pub const MAX_FILESYSTEM_SCAN_LIMIT: usize = 8_192;
/// Largest number of extension filters accepted by one filesystem generator.
pub const MAX_EXTENSION_FILTERS: usize = 32;
/// Largest extension filter length, excluding an optional leading dot.
pub const MAX_EXTENSION_LENGTH: usize = 64;

/// The argument location populated by a dynamic generator.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum GeneratorTarget {
    /// A zero-based positional argument at the containing command node.
    Positional(usize),
    /// Every positional argument at or after the zero-based starting index.
    PositionalsFrom(usize),
    /// The value consumed by the named option, such as `--format` or `-C`.
    OptionValue(String),
}

/// A supported source of local dynamic completion values.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum GeneratorKind {
    /// Local and remote Git branch names.
    GitBranches,
    /// Git remote names.
    GitRemotes,
    /// Git tag names.
    GitTags,
    /// Git stash references.
    GitStashes,
    /// Bounded recent Git commit identifiers and summaries.
    GitCommits,
    /// Git files relevant to the current repository state.
    GitFiles,
    /// Scripts declared by npm, pnpm, Yarn, or Bun project metadata.
    PackageScripts,
    /// Targets declared by the active Makefile.
    MakeTargets,
    /// Recipes declared by the active justfile.
    JustRecipes,
    /// Docker container names and identifiers.
    DockerContainers,
    /// Docker image names and identifiers.
    DockerImages,
    /// Concrete SSH host names, excluding wildcard and negated patterns.
    SshHosts,
    /// Directories known to zoxide.
    ZoxideDirectories,
    /// Packages supported by the current command and workspace context.
    Packages,
    /// Local process names and identifiers.
    Processes,
    /// Local service or unit names.
    Services,
    /// Names of variables in the current environment snapshot.
    EnvironmentVariables,
    /// File-type values supported by the current command.
    FileTypes,
    /// Values from one non-recursive, bounded filesystem lookup.
    Filesystem(FilesystemGenerator),
}

/// Filters and scan bounds for a filesystem generator.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FilesystemGenerator {
    /// Return directories only.
    pub directory_only: bool,
    /// Allowed file extensions, with or without one leading dot.
    ///
    /// Providers must continue to return directories so users can traverse into
    /// them before selecting an extension-matching file.
    pub extensions: Vec<String>,
    /// Maximum number of entries inspected in the resolved directory.
    pub max_entries: usize,
}

impl Default for FilesystemGenerator {
    fn default() -> Self {
        Self {
            directory_only: false,
            extensions: Vec::new(),
            max_entries: DEFAULT_FILESYSTEM_SCAN_LIMIT,
        }
    }
}

/// One validated request for dynamic values at a command argument location.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GeneratorSpec {
    /// Dynamic value source.
    pub kind: GeneratorKind,
    /// Argument location populated by the generated values.
    pub target: GeneratorTarget,
    /// Hard wall-clock execution budget.
    pub timeout: Duration,
    /// Maximum lifetime of a successful cached result.
    pub cache_ttl: Duration,
    /// Maximum number of returned values before ranking and merge.
    pub max_results: usize,
}

impl GeneratorSpec {
    /// Creates a generator request with conservative interactive defaults.
    #[must_use]
    pub const fn new(kind: GeneratorKind, target: GeneratorTarget) -> Self {
        Self {
            kind,
            target,
            timeout: DEFAULT_TIMEOUT,
            cache_ttl: DEFAULT_CACHE_TTL,
            max_results: DEFAULT_MAX_RESULTS,
        }
    }

    /// Validates the target, execution bounds, and source-specific metadata.
    ///
    /// # Errors
    ///
    /// Returns the first zero or excessive bound, malformed option target, or
    /// invalid filesystem extension.
    pub fn validate(&self) -> Result<(), GeneratorError> {
        validate_duration("timeout", self.timeout, MAX_TIMEOUT)?;
        validate_duration("cache_ttl", self.cache_ttl, MAX_CACHE_TTL)?;
        validate_count("max_results", self.max_results, MAX_RESULTS)?;
        validate_target(&self.target)?;

        if let GeneratorKind::Filesystem(filesystem) = &self.kind {
            validate_filesystem(filesystem)?;
        }

        Ok(())
    }
}

/// Validation failure for declarative generator metadata.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GeneratorError {
    /// Field containing the invalid value.
    pub field: &'static str,
    /// Human-readable reason suitable for catalog diagnostics.
    pub message: String,
}

impl GeneratorError {
    fn new(field: &'static str, message: impl Into<String>) -> Self {
        Self {
            field,
            message: message.into(),
        }
    }
}

impl fmt::Display for GeneratorError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.field, self.message)
    }
}

impl Error for GeneratorError {}

fn validate_duration(
    field: &'static str,
    value: Duration,
    maximum: Duration,
) -> Result<(), GeneratorError> {
    if value.is_zero() {
        return Err(GeneratorError::new(field, "must be greater than zero"));
    }
    if value > maximum {
        return Err(GeneratorError::new(
            field,
            format!("must not exceed {maximum:?}"),
        ));
    }
    Ok(())
}

fn validate_count(field: &'static str, value: usize, maximum: usize) -> Result<(), GeneratorError> {
    if value == 0 {
        return Err(GeneratorError::new(field, "must be greater than zero"));
    }
    if value > maximum {
        return Err(GeneratorError::new(
            field,
            format!("must not exceed {maximum}"),
        ));
    }
    Ok(())
}

fn validate_target(target: &GeneratorTarget) -> Result<(), GeneratorError> {
    let GeneratorTarget::OptionValue(option) = target else {
        return Ok(());
    };

    if option.len() < 2
        || !option.starts_with('-')
        || matches!(option.as_str(), "-" | "--")
        || option.contains('=')
        || option
            .chars()
            .any(|character| character.is_whitespace() || character.is_control())
    {
        return Err(GeneratorError::new(
            "target",
            format!("invalid option value target {option:?}"),
        ));
    }

    Ok(())
}

fn validate_filesystem(filesystem: &FilesystemGenerator) -> Result<(), GeneratorError> {
    validate_count(
        "kind.filesystem.max_entries",
        filesystem.max_entries,
        MAX_FILESYSTEM_SCAN_LIMIT,
    )?;

    if filesystem.extensions.len() > MAX_EXTENSION_FILTERS {
        return Err(GeneratorError::new(
            "kind.filesystem.extensions",
            format!("must contain at most {MAX_EXTENSION_FILTERS} filters"),
        ));
    }
    if filesystem.directory_only && !filesystem.extensions.is_empty() {
        return Err(GeneratorError::new(
            "kind.filesystem.extensions",
            "extension filters cannot be combined with directory-only lookup",
        ));
    }

    let mut extensions = BTreeSet::new();
    for extension in &filesystem.extensions {
        let normalized = extension.strip_prefix('.').unwrap_or(extension);
        if !valid_extension(normalized) {
            return Err(GeneratorError::new(
                "kind.filesystem.extensions",
                format!("invalid extension filter {extension:?}"),
            ));
        }
        if !extensions.insert(normalized.to_ascii_lowercase()) {
            return Err(GeneratorError::new(
                "kind.filesystem.extensions",
                format!("duplicate extension filter {extension:?}"),
            ));
        }
    }

    Ok(())
}

fn valid_extension(extension: &str) -> bool {
    !extension.is_empty()
        && extension.len() <= MAX_EXTENSION_LENGTH
        && extension.chars().all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '_' | '-' | '+')
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn positional(kind: GeneratorKind) -> GeneratorSpec {
        GeneratorSpec::new(kind, GeneratorTarget::Positional(0))
    }

    #[test]
    fn safe_defaults_validate_for_every_generator_kind() {
        let kinds = [
            GeneratorKind::GitBranches,
            GeneratorKind::GitRemotes,
            GeneratorKind::GitTags,
            GeneratorKind::GitStashes,
            GeneratorKind::GitCommits,
            GeneratorKind::GitFiles,
            GeneratorKind::PackageScripts,
            GeneratorKind::MakeTargets,
            GeneratorKind::JustRecipes,
            GeneratorKind::DockerContainers,
            GeneratorKind::DockerImages,
            GeneratorKind::SshHosts,
            GeneratorKind::ZoxideDirectories,
            GeneratorKind::Packages,
            GeneratorKind::Processes,
            GeneratorKind::Services,
            GeneratorKind::EnvironmentVariables,
            GeneratorKind::FileTypes,
            GeneratorKind::Filesystem(FilesystemGenerator::default()),
        ];

        for kind in kinds {
            let spec = positional(kind);
            assert_eq!(spec.timeout, DEFAULT_TIMEOUT);
            assert_eq!(spec.cache_ttl, DEFAULT_CACHE_TTL);
            assert_eq!(spec.max_results, DEFAULT_MAX_RESULTS);
            assert_eq!(spec.validate(), Ok(()));
        }
    }

    #[test]
    fn positional_range_and_option_value_targets_validate() {
        assert_eq!(positional(GeneratorKind::GitBranches).validate(), Ok(()));
        assert_eq!(
            GeneratorSpec::new(GeneratorKind::GitFiles, GeneratorTarget::PositionalsFrom(1),)
                .validate(),
            Ok(())
        );
        for option in ["-C", "--format", "--type-name"] {
            let spec = GeneratorSpec::new(
                GeneratorKind::FileTypes,
                GeneratorTarget::OptionValue(option.to_owned()),
            );
            assert_eq!(spec.validate(), Ok(()));
        }
    }

    #[test]
    fn invalid_option_value_targets_are_rejected() {
        for option in ["", "-", "--", "format", "--format=value", "--bad option"] {
            let spec = GeneratorSpec::new(
                GeneratorKind::FileTypes,
                GeneratorTarget::OptionValue(option.to_owned()),
            );
            assert_eq!(spec.validate().unwrap_err().field, "target");
        }
    }

    #[test]
    fn zero_and_excessive_generator_limits_are_rejected() {
        let mut spec = positional(GeneratorKind::Processes);

        spec.timeout = Duration::ZERO;
        assert_eq!(spec.validate().unwrap_err().field, "timeout");
        spec.timeout = MAX_TIMEOUT + Duration::from_nanos(1);
        assert_eq!(spec.validate().unwrap_err().field, "timeout");

        spec.timeout = DEFAULT_TIMEOUT;
        spec.cache_ttl = Duration::ZERO;
        assert_eq!(spec.validate().unwrap_err().field, "cache_ttl");
        spec.cache_ttl = MAX_CACHE_TTL + Duration::from_nanos(1);
        assert_eq!(spec.validate().unwrap_err().field, "cache_ttl");

        spec.cache_ttl = DEFAULT_CACHE_TTL;
        spec.max_results = 0;
        assert_eq!(spec.validate().unwrap_err().field, "max_results");
        spec.max_results = MAX_RESULTS + 1;
        assert_eq!(spec.validate().unwrap_err().field, "max_results");
    }

    #[test]
    fn filesystem_extensions_and_scan_bound_validate() {
        let filesystem = FilesystemGenerator {
            extensions: vec!["rs".to_owned(), ".toml".to_owned(), "c++".to_owned()],
            ..FilesystemGenerator::default()
        };
        assert_eq!(
            positional(GeneratorKind::Filesystem(filesystem)).validate(),
            Ok(())
        );

        for extension in ["", ".", "*.rs", "bad extension", "src/rs", "tar.gz"] {
            let filesystem = FilesystemGenerator {
                extensions: vec![extension.to_owned()],
                ..FilesystemGenerator::default()
            };
            assert_eq!(
                positional(GeneratorKind::Filesystem(filesystem))
                    .validate()
                    .unwrap_err()
                    .field,
                "kind.filesystem.extensions"
            );
        }

        let filesystem = FilesystemGenerator {
            max_entries: 0,
            ..FilesystemGenerator::default()
        };
        assert_eq!(
            positional(GeneratorKind::Filesystem(filesystem))
                .validate()
                .unwrap_err()
                .field,
            "kind.filesystem.max_entries"
        );

        let filesystem = FilesystemGenerator {
            max_entries: MAX_FILESYSTEM_SCAN_LIMIT + 1,
            ..FilesystemGenerator::default()
        };
        assert_eq!(
            positional(GeneratorKind::Filesystem(filesystem))
                .validate()
                .unwrap_err()
                .field,
            "kind.filesystem.max_entries"
        );
    }

    #[test]
    fn redundant_or_conflicting_filesystem_filters_are_rejected() {
        let duplicate = FilesystemGenerator {
            extensions: vec!["rs".to_owned(), ".RS".to_owned()],
            ..FilesystemGenerator::default()
        };
        assert_eq!(
            positional(GeneratorKind::Filesystem(duplicate))
                .validate()
                .unwrap_err()
                .field,
            "kind.filesystem.extensions"
        );

        let directory_only = FilesystemGenerator {
            directory_only: true,
            extensions: vec!["rs".to_owned()],
            ..FilesystemGenerator::default()
        };
        assert_eq!(
            positional(GeneratorKind::Filesystem(directory_only))
                .validate()
                .unwrap_err()
                .field,
            "kind.filesystem.extensions"
        );

        let excessive_count = FilesystemGenerator {
            extensions: vec!["rs".to_owned(); MAX_EXTENSION_FILTERS + 1],
            ..FilesystemGenerator::default()
        };
        assert_eq!(
            positional(GeneratorKind::Filesystem(excessive_count))
                .validate()
                .unwrap_err()
                .field,
            "kind.filesystem.extensions"
        );

        let excessive_length = FilesystemGenerator {
            extensions: vec!["r".repeat(MAX_EXTENSION_LENGTH + 1)],
            ..FilesystemGenerator::default()
        };
        assert_eq!(
            positional(GeneratorKind::Filesystem(excessive_length))
                .validate()
                .unwrap_err()
                .field,
            "kind.filesystem.extensions"
        );
    }
}
