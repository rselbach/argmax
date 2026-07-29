//! Dependency-free resolved configuration and validation.
//!
//! Parsing, environment precedence, and migration are deliberately outside this
//! module. A loader may accept legacy underscore spellings, but it must resolve
//! them into this single typed representation before validation.

mod resolve;

use std::collections::BTreeMap;
use std::error::Error;
use std::fmt;
use std::time::Duration;

use crate::keybindings::{
    KeybindingValidationErrors, KeybindingValidationProblem, ResolvedKeybindings,
};

pub use resolve::{
    CliOverrides, ENV_AI_ENABLED, ENV_AI_PROVIDER, ENV_CORE_DEBUG, ENV_CORE_MODE, ENV_CORE_SHELL,
    ENV_UI_GHOST_TEXT, ENV_UI_MAX_HEIGHT, ENV_UI_MAX_SUGGESTIONS, ENV_UPDATER_CHANNEL,
    ENV_UPDATER_CHECK_ON_STARTUP, ENV_UPDATER_INTERVAL, EnvironmentOverrides, OverrideError,
    OverrideErrors, resolve_settings,
};

/// Current configuration schema emitted by the Rust rewrite.
///
/// Version 2 deliberately leaves version 1 available as legacy migration input.
/// This model accepts only fully migrated version 2 settings.
pub const CURRENT_SCHEMA_VERSION: u32 = 2;

const MINUTE_SECONDS: u64 = 60;
const DAY_SECONDS: u64 = 24 * 60 * MINUTE_SECONDS;

/// Default interval between automatic update checks: 24 hours.
pub const DEFAULT_UPDATE_CHECK_INTERVAL: Duration = Duration::from_secs(DAY_SECONDS);
/// Shortest supported update interval: one minute.
///
/// Users who need no network checks should disable startup checks instead of
/// configuring a tight loop.
pub const MIN_UPDATE_CHECK_INTERVAL: Duration = Duration::from_secs(MINUTE_SECONDS);
/// Longest supported update interval: 30 days.
pub const MAX_UPDATE_CHECK_INTERVAL: Duration = Duration::from_secs(30 * DAY_SECONDS);

/// Smallest supported AI debounce in milliseconds.
pub const MIN_AI_DEBOUNCE_MS: u64 = 0;
/// Default AI debounce in milliseconds.
pub const DEFAULT_AI_DEBOUNCE_MS: u64 = 500;
/// Largest supported AI debounce in milliseconds.
pub const MAX_AI_DEBOUNCE_MS: u64 = 10_000;
/// Smallest positive interval between AI calls in milliseconds.
pub const MIN_AI_CALL_INTERVAL_MS: u64 = 1;
/// Default minimum interval between AI calls in milliseconds.
pub const DEFAULT_AI_CALL_INTERVAL_MS: u64 = 1_000;
/// Largest interval between AI calls in milliseconds.
pub const MAX_AI_CALL_INTERVAL_MS: u64 = 60_000;
/// Smallest positive provider timeout in milliseconds.
pub const MIN_AI_PROVIDER_TIMEOUT_MS: u64 = 1;
/// Default provider timeout in milliseconds.
pub const DEFAULT_AI_PROVIDER_TIMEOUT_MS: u64 = 2_000;
/// Largest provider timeout in milliseconds.
pub const MAX_AI_PROVIDER_TIMEOUT_MS: u64 = 60_000;

/// Marker used anywhere a compatibility plaintext credential would be shown.
pub const REDACTED_CREDENTIAL: &str = "<redacted>";

/// Fully resolved settings after defaults and precedence have been applied.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Settings {
    /// Shell-session behavior.
    pub core: Core,
    /// Suggestion rendering behavior.
    pub ui: Ui,
    /// User-configurable non-editing key names.
    pub keybindings: Keybindings,
    /// Git candidate behavior.
    pub git: Git,
    /// Background update-check behavior.
    pub updater: Updater,
    /// Optional AI completion behavior.
    pub ai: Ai,
}

impl Settings {
    /// Validates all known settings and returns every responsible field.
    ///
    /// Keybindings are validated against their terminal byte sequences, including
    /// protected controls, encoded aliases, and prefix conflicts. Incomplete
    /// provider stanzas are allowed while AI is disabled, which permits a
    /// commented template to be filled in incrementally.
    ///
    /// # Errors
    ///
    /// Returns field-specific errors for unsupported schema versions, invalid
    /// bounds, invalid keybinding names, or unusable enabled AI configuration.
    pub fn validate(&self) -> Result<(), ValidationErrors> {
        let mut errors = Vec::new();

        if self.core.version != CURRENT_SCHEMA_VERSION {
            errors.push(ValidationError::new(
                "core.version",
                ValidationProblem::UnsupportedSchema {
                    found: self.core.version,
                    supported: CURRENT_SCHEMA_VERSION,
                },
            ));
        }

        validate_inclusive(
            &mut errors,
            "ui.max-suggestions",
            u64::from(self.ui.max_suggestions),
            1,
            500,
        );
        validate_inclusive(
            &mut errors,
            "ui.max-height",
            u64::from(self.ui.max_height),
            3,
            50,
        );

        validate_keybindings(&self.keybindings, &mut errors);

        if !(MIN_UPDATE_CHECK_INTERVAL..=MAX_UPDATE_CHECK_INTERVAL)
            .contains(&self.updater.check_interval)
        {
            errors.push(ValidationError::new(
                "updater.check-interval",
                ValidationProblem::DurationOutOfRange {
                    value: self.updater.check_interval,
                    minimum: MIN_UPDATE_CHECK_INTERVAL,
                    maximum: MAX_UPDATE_CHECK_INTERVAL,
                },
            ));
        }

        validate_inclusive(
            &mut errors,
            "ai.debounce_ms",
            self.ai.debounce_ms,
            MIN_AI_DEBOUNCE_MS,
            MAX_AI_DEBOUNCE_MS,
        );
        validate_inclusive(
            &mut errors,
            "ai.min_interval_ms",
            self.ai.min_interval_ms,
            MIN_AI_CALL_INTERVAL_MS,
            MAX_AI_CALL_INTERVAL_MS,
        );

        for (name, provider) in &self.ai.providers {
            if name.trim().is_empty() {
                errors.push(ValidationError::new(
                    "ai.providers",
                    ValidationProblem::Blank,
                ));
            }
            validate_inclusive(
                &mut errors,
                format!("ai.providers.{name}.timeout_ms"),
                provider.timeout_ms,
                MIN_AI_PROVIDER_TIMEOUT_MS,
                MAX_AI_PROVIDER_TIMEOUT_MS,
            );
        }

        validate_ai_selection(&self.ai, &mut errors);

        if errors.is_empty() {
            Ok(())
        } else {
            Err(ValidationErrors::new(errors))
        }
    }
}

/// Core session settings.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Core {
    /// Managed schema version. The current value is 2.
    pub version: u32,
    /// Explicit shell override, or automatic shell selection.
    pub shell: Option<Shell>,
    /// Initial suggestion mode policy.
    pub mode: Mode,
    /// Whether private diagnostic logging is enabled.
    pub debug: bool,
    /// Whether an exact root alias expands when the user types a space.
    pub expand_alias: bool,
}

impl Default for Core {
    fn default() -> Self {
        Self {
            version: CURRENT_SCHEMA_VERSION,
            shell: None,
            mode: Mode::Last,
            debug: false,
            expand_alias: true,
        }
    }
}

/// Supported interactive shell override.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum Shell {
    /// Bourne Again Shell.
    Bash,
    /// Z shell.
    Zsh,
    /// Friendly Interactive Shell.
    Fish,
}

impl Shell {
    /// Returns the stable CLI and configuration spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Bash => "bash",
            Self::Zsh => "zsh",
            Self::Fish => "fish",
        }
    }
}

impl fmt::Display for Shell {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Initial suggestion mode policy.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Mode {
    /// Restore the last selected mode from state.
    Last,
    /// Always begin in specification mode.
    Spec,
    /// Always begin in history mode.
    History,
}

/// Suggestion rendering settings.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Ui {
    /// Shipped visual style.
    pub style: Style,
    /// Whether Nerd Font icon glyphs may be used.
    pub nerd_fonts: bool,
    /// Whether dot-prefixed filesystem candidates are included.
    pub hidden_files: bool,
    /// Whether the selected candidate's suffix is shown inline.
    pub ghost_text: bool,
    /// Maximum ranked results, inclusive range 1 through 500.
    pub max_suggestions: u16,
    /// Maximum visible rows, inclusive range 3 through 50.
    pub max_height: u16,
}

impl Default for Ui {
    fn default() -> Self {
        Self {
            style: Style::Modern,
            nerd_fonts: true,
            hidden_files: false,
            ghost_text: true,
            max_suggestions: 100,
            max_height: 15,
        }
    }
}

/// Shipped visual style.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Style {
    /// Richer icons and decoration.
    Modern,
    /// Restrained compatibility-oriented presentation.
    Classic,
}

/// Names for the two configurable bindings.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Keybindings {
    /// Mode switch binding name.
    pub toggle_mode: String,
    /// Session menu toggle binding name.
    pub toggle_menu: String,
}

impl Keybindings {
    /// Parses and validates both names into terminal byte sequences.
    ///
    /// # Errors
    ///
    /// Returns every responsible keybinding field when either name is invalid,
    /// protected, duplicated by encoding, or prefix-conflicting.
    pub fn resolve(&self) -> Result<ResolvedKeybindings, KeybindingValidationErrors> {
        ResolvedKeybindings::resolve(&self.toggle_mode, &self.toggle_menu)
    }
}

impl Default for Keybindings {
    fn default() -> Self {
        Self {
            toggle_mode: "ctrl+r".to_string(),
            toggle_menu: "shift+tab".to_string(),
        }
    }
}

/// Git completion filtering settings.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Git {
    /// Hide the active branch where choosing it is a no-op.
    pub filter_active_branch: bool,
    /// Merge equivalent local and remote branch rows.
    pub deduplicate_branches: bool,
}

impl Default for Git {
    fn default() -> Self {
        Self {
            filter_active_branch: true,
            deduplicate_branches: true,
        }
    }
}

/// Background update-check settings.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Updater {
    /// Whether startup may launch a background check.
    pub check_on_startup: bool,
    /// Release channel to inspect.
    pub channel: UpdateChannel,
    /// Time between checks, from one minute through 30 days.
    pub check_interval: Duration,
}

impl Default for Updater {
    fn default() -> Self {
        Self {
            check_on_startup: true,
            channel: UpdateChannel::Stable,
            check_interval: DEFAULT_UPDATE_CHECK_INTERVAL,
        }
    }
}

/// Supported update release channel.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UpdateChannel {
    /// Production releases.
    Stable,
    /// Pre-release builds.
    Nightly,
}

/// Optional AI completion settings.
#[derive(Clone, Debug, PartialEq)]
pub struct Ai {
    /// Whether provider requests are allowed.
    pub enabled: bool,
    /// Selected name in [`Self::providers`], or no selected provider.
    pub provider: Option<String>,
    /// Explicit request data boundary.
    pub context_level: AiContextLevel,
    /// Delay after the latest buffer change, in milliseconds.
    pub debounce_ms: u64,
    /// Minimum spacing between provider calls, in milliseconds.
    pub min_interval_ms: u64,
    /// Named OpenAI-compatible provider definitions.
    pub providers: BTreeMap<String, AiProvider>,
}

impl Default for Ai {
    fn default() -> Self {
        Self {
            enabled: false,
            provider: None,
            context_level: AiContextLevel::Minimal,
            debounce_ms: DEFAULT_AI_DEBOUNCE_MS,
            min_interval_ms: DEFAULT_AI_CALL_INTERVAL_MS,
            providers: BTreeMap::new(),
        }
    }
}

/// AI request data boundary.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AiContextLevel {
    /// Input buffer plus shell and operating-system metadata only.
    Minimal,
    /// Minimal data plus bounded workspace metadata.
    Workspace,
    /// Workspace data plus bounded Git metadata.
    Full,
}

/// One named OpenAI-compatible provider.
#[derive(Clone, Debug, PartialEq)]
pub struct AiProvider {
    /// Compatibility protocol inherited by this provider.
    pub inherited_from: ProviderProtocol,
    /// Provider base URL or chat-completion endpoint.
    pub endpoint: Option<String>,
    /// Preferred environment variable containing a credential.
    pub api_key_env: Option<String>,
    /// Discouraged plaintext compatibility credential.
    pub api_key: Option<Credential>,
    /// Provider model identifier.
    pub model: Option<String>,
    /// Positive bounded request timeout in milliseconds.
    pub timeout_ms: u64,
    /// Provider-specific request data that cannot replace safety fields.
    pub extra_request_body: BTreeMap<String, ExtraRequestValue>,
}

impl Default for AiProvider {
    fn default() -> Self {
        Self {
            inherited_from: ProviderProtocol::OpenAi,
            endpoint: None,
            api_key_env: None,
            api_key: None,
            model: None,
            timeout_ms: DEFAULT_AI_PROVIDER_TIMEOUT_MS,
            extra_request_body: BTreeMap::new(),
        }
    }
}

impl AiProvider {
    /// Returns a display-safe marker when a plaintext API key is configured.
    #[must_use]
    pub fn redacted_api_key(&self) -> Option<&'static str> {
        redact_credential(self.api_key.as_ref())
    }
}

/// Provider protocol retained for compatibility with inherited provider stanzas.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ProviderProtocol {
    /// OpenAI-compatible chat completions.
    #[default]
    OpenAi,
}

/// Dependency-free value accepted in a provider's extra request body.
#[derive(Clone, Debug, PartialEq)]
pub enum ExtraRequestValue {
    /// UTF-8 string.
    String(String),
    /// Signed integer.
    Integer(i64),
    /// Finite floating-point number. A later encoder rejects non-finite values.
    Float(f64),
    /// Boolean.
    Boolean(bool),
    /// Nested array.
    Array(Vec<Self>),
    /// Nested table/object.
    Table(BTreeMap<String, Self>),
}

/// Plaintext credential with redacted debug output.
///
/// Access requires the deliberately explicit [`Self::expose_secret`] method so
/// ordinary diagnostics cannot accidentally format the credential.
#[derive(Clone, Eq, PartialEq)]
pub struct Credential(String);

impl Credential {
    /// Wraps a compatibility plaintext credential.
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// Exposes the credential only to the provider request boundary.
    #[must_use]
    pub fn expose_secret(&self) -> &str {
        &self.0
    }

    /// Reports whether a configured value contains no non-whitespace text.
    #[must_use]
    pub fn is_blank(&self) -> bool {
        self.0.trim().is_empty()
    }

    /// Returns the stable display-safe replacement.
    #[must_use]
    pub const fn redacted(&self) -> &'static str {
        REDACTED_CREDENTIAL
    }
}

impl fmt::Debug for Credential {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(REDACTED_CREDENTIAL)
    }
}

/// Returns a display-safe marker for an optional credential.
#[must_use]
pub fn redact_credential(credential: Option<&Credential>) -> Option<&'static str> {
    credential.map(Credential::redacted)
}

/// Exact reason one resolved field failed validation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ValidationProblem {
    /// The document has not been migrated to the current schema.
    UnsupportedSchema {
        /// Schema version found in the resolved input.
        found: u32,
        /// Only schema version accepted by this model.
        supported: u32,
    },
    /// The value contains no non-whitespace text.
    Blank,
    /// The value duplicates another field.
    Duplicate {
        /// Other responsible field.
        other_field: &'static str,
    },
    /// An integer lies outside an inclusive bound.
    OutOfRange {
        /// Invalid value.
        value: u64,
        /// Inclusive lower bound.
        minimum: u64,
        /// Inclusive upper bound.
        maximum: u64,
    },
    /// A duration lies outside an inclusive bound.
    DurationOutOfRange {
        /// Invalid value.
        value: Duration,
        /// Inclusive lower bound.
        minimum: Duration,
        /// Inclusive upper bound.
        maximum: Duration,
    },
    /// A required optional value was absent.
    Missing,
    /// A selected provider name does not exist.
    UnknownProvider {
        /// Provider name selected by `ai.provider`.
        name: String,
    },
    /// A binding is malformed, unsupported, protected, or prefix-conflicting.
    Keybinding {
        /// Structured terminal-level binding failure.
        problem: KeybindingValidationProblem,
    },
}

/// One field-specific validation failure.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ValidationError {
    /// Dotted configuration field path.
    pub field: String,
    /// Structured reason the field is invalid.
    pub problem: ValidationProblem,
}

impl ValidationError {
    fn new(field: impl Into<String>, problem: ValidationProblem) -> Self {
        Self {
            field: field.into(),
            problem,
        }
    }
}

impl fmt::Display for ValidationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: ", self.field)?;
        match &self.problem {
            ValidationProblem::UnsupportedSchema { found, supported } => {
                write!(
                    formatter,
                    "schema version {found} is unsupported; expected {supported}"
                )
            }
            ValidationProblem::Blank => formatter.write_str("must not be blank"),
            ValidationProblem::Duplicate { other_field } => {
                write!(formatter, "duplicates {other_field}")
            }
            ValidationProblem::OutOfRange {
                value,
                minimum,
                maximum,
            } => write!(
                formatter,
                "{value} is outside the inclusive range {minimum}..={maximum}"
            ),
            ValidationProblem::DurationOutOfRange {
                value,
                minimum,
                maximum,
            } => write!(
                formatter,
                "{value:?} is outside the inclusive range {minimum:?}..={maximum:?}"
            ),
            ValidationProblem::Missing => formatter.write_str("is required"),
            ValidationProblem::UnknownProvider { name } => {
                write!(formatter, "references unknown provider {name:?}")
            }
            ValidationProblem::Keybinding { problem } => problem.fmt(formatter),
        }
    }
}

/// Complete set of invalid fields in one resolved configuration.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ValidationErrors {
    errors: Vec<ValidationError>,
}

impl ValidationErrors {
    fn new(errors: Vec<ValidationError>) -> Self {
        Self { errors }
    }

    /// Returns the individual field-specific errors.
    #[must_use]
    pub fn errors(&self) -> &[ValidationError] {
        &self.errors
    }

    /// Consumes the collection and returns the individual errors.
    #[must_use]
    pub fn into_errors(self) -> Vec<ValidationError> {
        self.errors
    }
}

impl fmt::Display for ValidationErrors {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (index, error) in self.errors.iter().enumerate() {
            if index != 0 {
                formatter.write_str("; ")?;
            }
            write!(formatter, "{error}")?;
        }
        Ok(())
    }
}

impl Error for ValidationErrors {}

fn validate_keybindings(keybindings: &Keybindings, errors: &mut Vec<ValidationError>) {
    let Err(binding_errors) = keybindings.resolve() else {
        return;
    };

    for error in binding_errors.into_errors() {
        let problem = match error.problem() {
            KeybindingValidationProblem::Blank => ValidationProblem::Blank,
            KeybindingValidationProblem::Duplicate { other_field } => {
                ValidationProblem::Duplicate {
                    other_field: other_field.field(),
                }
            }
            problem => ValidationProblem::Keybinding { problem },
        };
        errors.push(ValidationError::new(error.field_path(), problem));
    }
}

fn validate_ai_selection(ai: &Ai, errors: &mut Vec<ValidationError>) {
    let Some(name) = ai.provider.as_deref() else {
        if ai.enabled {
            errors.push(ValidationError::new(
                "ai.provider",
                ValidationProblem::Missing,
            ));
        }
        return;
    };

    if name.trim().is_empty() {
        errors.push(ValidationError::new(
            "ai.provider",
            ValidationProblem::Blank,
        ));
        return;
    }

    if !ai.enabled {
        return;
    }

    let Some(provider) = ai.providers.get(name) else {
        errors.push(ValidationError::new(
            "ai.provider",
            ValidationProblem::UnknownProvider {
                name: name.to_string(),
            },
        ));
        return;
    };

    validate_required_text(
        errors,
        format!("ai.providers.{name}.endpoint"),
        provider.endpoint.as_deref(),
    );
    validate_required_text(
        errors,
        format!("ai.providers.{name}.model"),
        provider.model.as_deref(),
    );
}

fn validate_required_text(
    errors: &mut Vec<ValidationError>,
    field: impl Into<String>,
    value: Option<&str>,
) {
    let field = field.into();
    match value {
        None => errors.push(ValidationError::new(field, ValidationProblem::Missing)),
        Some(value) if value.trim().is_empty() => {
            errors.push(ValidationError::new(field, ValidationProblem::Blank));
        }
        Some(_) => {}
    }
}

fn validate_inclusive(
    errors: &mut Vec<ValidationError>,
    field: impl Into<String>,
    value: u64,
    minimum: u64,
    maximum: u64,
) {
    if !(minimum..=maximum).contains(&value) {
        errors.push(ValidationError::new(
            field,
            ValidationProblem::OutOfRange {
                value,
                minimum,
                maximum,
            },
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn greendale_defaults_match_the_required_schema() {
        let settings = Settings::default();

        assert_eq!(settings.core.version, 2);
        assert_eq!(settings.core.shell, None);
        assert_eq!(settings.core.mode, Mode::Last);
        assert!(!settings.core.debug);
        assert!(settings.core.expand_alias);
        assert_eq!(settings.ui.style, Style::Modern);
        assert!(settings.ui.nerd_fonts);
        assert!(!settings.ui.hidden_files);
        assert!(settings.ui.ghost_text);
        assert_eq!(settings.ui.max_suggestions, 100);
        assert_eq!(settings.ui.max_height, 15);
        assert_eq!(settings.keybindings.toggle_mode, "ctrl+r");
        assert_eq!(settings.keybindings.toggle_menu, "shift+tab");
        assert!(settings.git.filter_active_branch);
        assert!(settings.git.deduplicate_branches);
        assert!(settings.updater.check_on_startup);
        assert_eq!(settings.updater.channel, UpdateChannel::Stable);
        assert_eq!(
            settings.updater.check_interval,
            DEFAULT_UPDATE_CHECK_INTERVAL
        );
        assert!(!settings.ai.enabled);
        assert_eq!(settings.ai.provider, None);
        assert_eq!(settings.ai.context_level, AiContextLevel::Minimal);
        assert_eq!(settings.ai.debounce_ms, 500);
        assert_eq!(settings.ai.min_interval_ms, 1_000);
        assert!(settings.ai.providers.is_empty());
        assert_eq!(settings.validate(), Ok(()));
    }

    #[test]
    fn dean_reports_each_responsible_bounded_field() {
        enum Case {
            Schema(u32),
            Suggestions(u16),
            Height(u16),
            Update(Duration),
            Debounce(u64),
            Interval(u64),
            Timeout(u64),
        }

        let cases = BTreeMap::from([
            (
                "ai debounce above maximum",
                ("ai.debounce_ms", Case::Debounce(MAX_AI_DEBOUNCE_MS + 1)),
            ),
            (
                "ai interval zero",
                ("ai.min_interval_ms", Case::Interval(0)),
            ),
            (
                "provider timeout zero",
                ("ai.providers.greendale.timeout_ms", Case::Timeout(0)),
            ),
            ("schema one", ("core.version", Case::Schema(1))),
            (
                "suggestions above maximum",
                ("ui.max-suggestions", Case::Suggestions(501)),
            ),
            (
                "terminal height too short",
                ("ui.max-height", Case::Height(2)),
            ),
            (
                "update interval too long",
                (
                    "updater.check-interval",
                    Case::Update(MAX_UPDATE_CHECK_INTERVAL + Duration::from_secs(1)),
                ),
            ),
        ]);

        for (label, (want_field, case)) in cases {
            let mut settings = Settings::default();
            match case {
                Case::Schema(value) => settings.core.version = value,
                Case::Suggestions(value) => settings.ui.max_suggestions = value,
                Case::Height(value) => settings.ui.max_height = value,
                Case::Update(value) => settings.updater.check_interval = value,
                Case::Debounce(value) => settings.ai.debounce_ms = value,
                Case::Interval(value) => settings.ai.min_interval_ms = value,
                Case::Timeout(value) => {
                    settings.ai.providers.insert(
                        "greendale".to_string(),
                        AiProvider {
                            timeout_ms: value,
                            ..AiProvider::default()
                        },
                    );
                }
            }

            let errors = settings.validate().expect_err(label);
            assert!(
                errors
                    .errors()
                    .iter()
                    .any(|error| error.field == want_field),
                "{label}: {errors}"
            );
        }
    }

    #[test]
    fn troy_accepts_each_inclusive_boundary() {
        let cases = BTreeMap::from([
            (
                "minimum",
                (
                    1,
                    3,
                    MIN_UPDATE_CHECK_INTERVAL,
                    MIN_AI_DEBOUNCE_MS,
                    MIN_AI_CALL_INTERVAL_MS,
                    MIN_AI_PROVIDER_TIMEOUT_MS,
                ),
            ),
            (
                "maximum",
                (
                    500,
                    50,
                    MAX_UPDATE_CHECK_INTERVAL,
                    MAX_AI_DEBOUNCE_MS,
                    MAX_AI_CALL_INTERVAL_MS,
                    MAX_AI_PROVIDER_TIMEOUT_MS,
                ),
            ),
        ]);

        for (label, (suggestions, height, update, debounce, interval, timeout)) in cases {
            let mut settings = Settings::default();
            settings.ui.max_suggestions = suggestions;
            settings.ui.max_height = height;
            settings.updater.check_interval = update;
            settings.ai.debounce_ms = debounce;
            settings.ai.min_interval_ms = interval;
            settings.ai.providers.insert(
                "greendale".to_string(),
                AiProvider {
                    timeout_ms: timeout,
                    ..AiProvider::default()
                },
            );

            assert_eq!(settings.validate(), Ok(()), "{label}");
        }
    }

    #[test]
    fn annie_rejects_blank_and_duplicate_binding_names() {
        let cases = BTreeMap::from([
            ("blank menu", ("ctrl+r", " ", "keybindings.toggle-menu")),
            ("blank mode", ("", "shift+tab", "keybindings.toggle-mode")),
            ("duplicate", ("ctrl+r", "ctrl+r", "keybindings.toggle-menu")),
        ]);

        for (label, (mode, menu, want_field)) in cases {
            let settings = Settings {
                keybindings: Keybindings {
                    toggle_mode: mode.to_string(),
                    toggle_menu: menu.to_string(),
                },
                ..Settings::default()
            };
            let errors = settings.validate().expect_err(label);
            assert!(
                errors
                    .errors()
                    .iter()
                    .any(|error| error.field == want_field),
                "{label}: {errors}"
            );
        }
    }

    #[test]
    fn abed_validates_keybindings_by_terminal_encoding() {
        let cases = [
            (
                "encoded duplicate",
                "ctrl+space",
                "ctrl+@",
                "keybindings.toggle-menu",
                ValidationProblem::Duplicate {
                    other_field: "keybindings.toggle-mode",
                },
            ),
            (
                "fixed control",
                "ctrl+a",
                "shift+tab",
                "keybindings.toggle-mode",
                ValidationProblem::Keybinding {
                    problem: KeybindingValidationProblem::FixedControl {
                        control: crate::keybindings::FixedControl::LineEditing,
                    },
                },
            ),
            (
                "prefix conflict",
                "escape",
                "shift+tab",
                "keybindings.toggle-menu",
                ValidationProblem::Keybinding {
                    problem: KeybindingValidationProblem::PrefixConflict {
                        other_field: crate::keybindings::KeybindingAction::ToggleMode,
                    },
                },
            ),
            (
                "unsupported",
                "ctrl+rr",
                "shift+tab",
                "keybindings.toggle-mode",
                ValidationProblem::Keybinding {
                    problem: KeybindingValidationProblem::Unsupported,
                },
            ),
        ];

        for (label, mode, menu, field, problem) in cases {
            let settings = Settings {
                keybindings: Keybindings {
                    toggle_mode: mode.to_owned(),
                    toggle_menu: menu.to_owned(),
                },
                ..Settings::default()
            };
            let errors = settings.validate().expect_err(label);
            assert!(
                errors
                    .errors()
                    .iter()
                    .any(|error| error.field == field && error.problem == problem),
                "{label}: {errors}"
            );
        }
    }

    #[test]
    fn abed_requires_a_complete_selected_provider_only_when_enabled() {
        enum Case {
            DisabledIncomplete,
            MissingSelection,
            UnknownSelection,
            MissingEndpoint,
            BlankModel,
            Complete,
        }

        let cases = BTreeMap::from([
            ("blank model", Case::BlankModel),
            ("complete provider", Case::Complete),
            ("disabled incomplete provider", Case::DisabledIncomplete),
            ("missing endpoint", Case::MissingEndpoint),
            ("missing selection", Case::MissingSelection),
            ("unknown selection", Case::UnknownSelection),
        ]);

        for (label, case) in cases {
            let mut settings = Settings::default();
            settings.ai.enabled = !matches!(case, Case::DisabledIncomplete);
            settings.ai.provider = match case {
                Case::MissingSelection => None,
                Case::UnknownSelection => Some("city-college".to_string()),
                _ => Some("greendale".to_string()),
            };
            if !matches!(case, Case::MissingSelection | Case::UnknownSelection) {
                let provider = AiProvider {
                    endpoint: match case {
                        Case::MissingEndpoint | Case::DisabledIncomplete => None,
                        _ => Some("http://127.0.0.1:11434/v1".to_string()),
                    },
                    model: match case {
                        Case::BlankModel => Some(" ".to_string()),
                        Case::Complete => Some("dean-v2".to_string()),
                        _ => None,
                    },
                    ..AiProvider::default()
                };
                settings
                    .ai
                    .providers
                    .insert("greendale".to_string(), provider);
            }

            let result = settings.validate();
            if matches!(case, Case::DisabledIncomplete | Case::Complete) {
                assert_eq!(result, Ok(()), "{label}");
            } else {
                let errors = result.expect_err(label);
                assert!(!errors.errors().is_empty(), "{label}");
                assert!(
                    errors
                        .errors()
                        .iter()
                        .all(|error| error.field.starts_with("ai.")),
                    "{label}: {errors}"
                );
            }
        }
    }

    #[test]
    fn shirley_keeps_compatibility_credentials_redacted() {
        let secret = Credential::new("chang-loves-security");
        let provider = AiProvider {
            inherited_from: ProviderProtocol::OpenAi,
            api_key_env: Some("GREENDALE_API_KEY".to_string()),
            api_key: Some(secret.clone()),
            extra_request_body: BTreeMap::from([(
                "temperature".to_string(),
                ExtraRequestValue::Float(0.2),
            )]),
            ..AiProvider::default()
        };

        assert_eq!(secret.expose_secret(), "chang-loves-security");
        assert_eq!(provider.redacted_api_key(), Some(REDACTED_CREDENTIAL));
        assert!(!format!("{provider:?}").contains("chang-loves-security"));
        assert!(format!("{provider:?}").contains(REDACTED_CREDENTIAL));
        assert_eq!(provider.inherited_from, ProviderProtocol::OpenAi);
        assert_eq!(provider.api_key_env.as_deref(), Some("GREENDALE_API_KEY"));
        assert_eq!(provider.extra_request_body.len(), 1);
    }
}
