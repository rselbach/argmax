//! Bounded TOML parsing and redacted rendering for user configuration.

use std::collections::BTreeMap;
use std::error::Error;
use std::fmt;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use super::{
    AiContextLevel, AiProvider, CURRENT_SCHEMA_VERSION, Credential, ExtraRequestValue, Mode,
    ProviderProtocol, REDACTED_CREDENTIAL, Settings, Shell, Style, UpdateChannel, ValidationErrors,
    ValidationProblem,
};

/// Maximum accepted configuration-file size: one MiB.
pub const MAX_CONFIG_BYTES: usize = 1024 * 1024;
/// Maximum unknown/compatibility notices retained from one document.
pub const MAX_CONFIG_WARNINGS: usize = 128;
/// Maximum aggregate UTF-8 bytes retained across warning key paths.
pub const MAX_CONFIG_WARNING_BYTES: usize = 16 * 1024;
/// Maximum UTF-8 bytes retained for one warning key path.
pub const MAX_CONFIG_WARNING_PATH_BYTES: usize = 512;

const MAX_CONFIG_PROBLEM_FIELD_BYTES: usize = 512;
const TRUNCATED_WARNING_PATH: &str = "<additional-keys>";

/// Commented configuration written by `argmax config init`.
pub const DEFAULT_CONFIG_TEMPLATE: &str = r#"# argmax configuration
# AI is disabled by default. If enabled, the exact input buffer is sent to the
# configured endpoint and may itself contain commands, paths, or sensitive
# values. Broader workspace or Git context requires an explicit context-level
# change.

[core]
version = 2
shell = ""
mode = "last"
debug = false
expand-alias = true
# Runs an uncurated PATH program as "<program> __complete ..." while typing to
# infer its completions. Programs that are not Cobra commands see an ordinary
# invocation, so enable this only where every program on PATH is trusted.
infer-completions = false

[ui]
style = "modern"
nerd-fonts = true
hidden-files = false
ghost-text = true
max-suggestions = 100
max-height = 15

[keybindings]
toggle-mode = "ctrl+r"
toggle-menu = "shift+tab"

[git]
filter-active-branch = true
deduplicate-branches = true

[updater]
check-on-startup = true
channel = "stable"
check-interval = "24h"

[ai]
enabled = false
provider = ""
context-level = "minimal"
debounce_ms = 500
min_interval_ms = 1000

# [ai.providers.openai]
# inherited_from = "openai"
# endpoint = "https://api.openai.com/v1"
# api_key_env = "OPENAI_API_KEY"
# model = "your-model"
# timeout_ms = 2000
"#;

/// Parsed settings plus non-fatal compatibility and unknown-key notices.
#[derive(Clone, Debug, PartialEq)]
pub struct ConfigDocument {
    /// Fully migrated and validated settings.
    pub settings: Settings,
    /// Source schema found in the file. Missing versions are legacy version 1.
    pub source_schema: u32,
    /// Unknown or ignored compatibility fields.
    pub warnings: Vec<ConfigWarning>,
}

impl ConfigDocument {
    /// Whether this document should be backed up and rewritten as the current schema.
    #[must_use]
    pub const fn needs_migration(&self) -> bool {
        self.source_schema != CURRENT_SCHEMA_VERSION
    }
}

/// Non-fatal configuration notice. Values are intentionally never retained.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ConfigWarning {
    /// Stable dotted key path.
    pub path: String,
    /// Display-safe reason.
    pub reason: &'static str,
}

/// One field-specific configuration failure.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ConfigProblem {
    /// Stable dotted field path.
    pub field: String,
    /// Display-safe expected value or invariant.
    pub message: String,
}

impl ConfigProblem {
    fn new(field: impl Into<String>, message: impl Into<String>) -> Self {
        let field = field.into();
        Self {
            field: safe_problem_field(&field),
            message: message.into(),
        }
    }
}

impl fmt::Display for ConfigProblem {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.field, self.message)
    }
}

/// Aggregate field failures discovered after syntactic TOML parsing.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ConfigProblems {
    problems: Vec<ConfigProblem>,
}

impl ConfigProblems {
    /// Individual failures in deterministic document order.
    #[must_use]
    pub fn problems(&self) -> &[ConfigProblem] {
        &self.problems
    }
}

impl fmt::Display for ConfigProblems {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (index, problem) in self.problems.iter().enumerate() {
            if index != 0 {
                formatter.write_str("; ")?;
            }
            write!(formatter, "{problem}")?;
        }
        Ok(())
    }
}

impl Error for ConfigProblems {}

/// Safe configuration parse or render failure.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ConfigFileError {
    /// Input exceeded [`MAX_CONFIG_BYTES`].
    TooLarge { bytes: usize },
    /// TOML syntax or type mismatch. Input text is deliberately omitted.
    InvalidToml,
    /// Syntactically valid fields violated the schema.
    Invalid(ConfigProblems),
    /// A validated settings value could not be represented as TOML.
    Render,
}

impl fmt::Display for ConfigFileError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooLarge { bytes } => write!(
                formatter,
                "configuration is {bytes} bytes; maximum is {MAX_CONFIG_BYTES}"
            ),
            Self::InvalidToml => formatter.write_str("configuration is not valid TOML"),
            Self::Invalid(problems) => write!(formatter, "invalid configuration: {problems}"),
            Self::Render => formatter.write_str("resolved configuration could not be rendered"),
        }
    }
}

impl Error for ConfigFileError {}

/// Parses, migrates, and validates one bounded TOML document.
///
/// Unknown keys are retained only as key-path warnings. Missing schema versions
/// are interpreted as version 1 for compatibility. Values are never included in
/// debug output or syntax errors.
///
/// # Errors
///
/// Returns a safe error for oversized input, invalid TOML, unsupported schema
/// values, invalid fields, or resolved-setting validation failures.
pub fn parse_config(input: &str) -> Result<ConfigDocument, ConfigFileError> {
    if input.len() > MAX_CONFIG_BYTES {
        return Err(ConfigFileError::TooLarge { bytes: input.len() });
    }

    let raw: RawConfig = toml::from_str(input).map_err(|_| ConfigFileError::InvalidToml)?;
    let mut warnings = WarningCollector::default();
    collect_unknown("", &raw.unknown, &mut warnings);
    collect_unknown("core", &raw.core.unknown, &mut warnings);
    collect_unknown("ui", &raw.ui.unknown, &mut warnings);
    collect_unknown("keybindings", &raw.keybindings.unknown, &mut warnings);
    collect_unknown("git", &raw.git.unknown, &mut warnings);
    collect_unknown("updater", &raw.updater.unknown, &mut warnings);
    collect_unknown("ai", &raw.ai.unknown, &mut warnings);
    for (name, provider) in &raw.ai.providers {
        collect_unknown(
            &format!("ai.providers.{name}"),
            &provider.unknown,
            &mut warnings,
        );
    }
    if raw.ai.suggest_on_empty.is_some() {
        warnings.push("ai.suggest_on_empty", "accepted for migration but inactive");
    }

    let source_schema = raw.core.version.unwrap_or(1);
    let mut problems = Vec::new();
    if !matches!(source_schema, 1 | CURRENT_SCHEMA_VERSION) {
        problems.push(ConfigProblem::new(
            "core.version",
            format!("supported schema is 1 or {CURRENT_SCHEMA_VERSION}"),
        ));
    }

    let mut settings = Settings::default();
    apply_core(&raw.core, &mut settings, &mut problems);
    apply_ui(&raw.ui, &mut settings, &mut problems);
    apply_keybindings(&raw.keybindings, &mut settings);
    apply_git(&raw.git, &mut settings);
    apply_updater(&raw.updater, &mut settings, &mut problems);
    apply_ai(raw.ai, &mut settings, &mut problems);
    settings.core.version = CURRENT_SCHEMA_VERSION;

    if let Err(errors) = settings.validate() {
        append_validation_errors(errors, &mut problems);
    }
    if !problems.is_empty() {
        return Err(ConfigFileError::Invalid(ConfigProblems { problems }));
    }

    Ok(ConfigDocument {
        settings,
        source_schema,
        warnings: warnings.into_warnings(),
    })
}

/// Renders a complete, current-schema, secret-redacted TOML document.
///
/// # Errors
///
/// Returns an error if `settings` is invalid or serialization fails.
pub fn render_resolved_config(settings: &Settings) -> Result<String, ConfigFileError> {
    render_config(settings, true)
}

pub(super) fn render_config_for_storage(settings: &Settings) -> Result<String, ConfigFileError> {
    render_config(settings, false)
}

fn render_config(settings: &Settings, redact: bool) -> Result<String, ConfigFileError> {
    settings
        .validate()
        .map_err(|errors| ConfigFileError::Invalid(problems_from_validation(errors)))?;
    let output = OutputConfig::new(settings, redact);
    toml::to_string_pretty(&output).map_err(|_| ConfigFileError::Render)
}

fn append_validation_errors(errors: ValidationErrors, problems: &mut Vec<ConfigProblem>) {
    problems.extend(errors.into_errors().into_iter().map(|error| {
        let message = match error.problem {
            ValidationProblem::UnsupportedSchema { found, supported } => {
                format!("schema version {found} is unsupported; expected {supported}")
            }
            ValidationProblem::Blank => "must not be blank".to_owned(),
            ValidationProblem::Duplicate { other_field } => {
                format!("duplicates {other_field}")
            }
            ValidationProblem::OutOfRange {
                value,
                minimum,
                maximum,
            } => format!("{value} is outside the inclusive range {minimum}..={maximum}"),
            ValidationProblem::DurationOutOfRange {
                value,
                minimum,
                maximum,
            } => format!("{value:?} is outside the inclusive range {minimum:?}..={maximum:?}"),
            ValidationProblem::Missing => "is required".to_owned(),
            ValidationProblem::UnknownProvider { .. } => {
                "references an unknown provider".to_owned()
            }
            ValidationProblem::Keybinding { problem } => problem.to_string(),
        };
        ConfigProblem::new(error.field, message)
    }));
}

fn safe_problem_field(field: &str) -> String {
    if field.starts_with("ai.providers.") {
        for suffix in ["timeout_ms", "endpoint", "model", "inherited_from"] {
            if field.ends_with(&format!(".{suffix}")) {
                return format!("ai.providers.<provider>.{suffix}");
            }
        }
        if field.contains(".extra_request_body.") {
            return "ai.providers.<provider>.extra_request_body.<field>".to_owned();
        }
        return "ai.providers.<provider>".to_owned();
    }
    bounded_text(field, MAX_CONFIG_PROBLEM_FIELD_BYTES)
}

fn problems_from_validation(errors: ValidationErrors) -> ConfigProblems {
    let mut problems = Vec::new();
    append_validation_errors(errors, &mut problems);
    ConfigProblems { problems }
}

fn collect_unknown(
    prefix: &str,
    unknown: &BTreeMap<String, toml::Value>,
    warnings: &mut WarningCollector,
) {
    for key in unknown.keys() {
        warnings.push_joined(prefix, key, "unknown key ignored");
    }
}

#[derive(Default)]
struct WarningCollector {
    warnings: Vec<ConfigWarning>,
    path_bytes: usize,
    exhausted: bool,
}

impl WarningCollector {
    fn push(&mut self, path: &str, reason: &'static str) {
        self.push_joined("", path, reason);
    }

    fn push_joined(&mut self, prefix: &str, key: &str, reason: &'static str) {
        if self.exhausted
            || self.warnings.len() >= MAX_CONFIG_WARNINGS
            || self.path_bytes >= MAX_CONFIG_WARNING_BYTES
        {
            self.exhausted = true;
            return;
        }

        let remaining = MAX_CONFIG_WARNING_BYTES - self.path_bytes;
        let limit = remaining.min(MAX_CONFIG_WARNING_PATH_BYTES);
        let path = bounded_joined_path(prefix, key, limit);
        if path.is_empty() {
            self.exhausted = true;
            return;
        }
        self.path_bytes += path.len();
        self.warnings.push(ConfigWarning { path, reason });
    }

    fn into_warnings(self) -> Vec<ConfigWarning> {
        let Self {
            mut warnings,
            mut path_bytes,
            exhausted,
        } = self;
        if exhausted {
            while warnings.len() >= MAX_CONFIG_WARNINGS
                || path_bytes + TRUNCATED_WARNING_PATH.len() > MAX_CONFIG_WARNING_BYTES
            {
                let Some(removed) = warnings.pop() else {
                    break;
                };
                path_bytes -= removed.path.len();
            }
            warnings.push(ConfigWarning {
                path: TRUNCATED_WARNING_PATH.to_owned(),
                reason: "additional unknown keys omitted",
            });
        }
        warnings
    }
}

fn bounded_joined_path(prefix: &str, key: &str, limit: usize) -> String {
    let needs_separator = !prefix.is_empty();
    let full_bytes = prefix
        .len()
        .saturating_add(usize::from(needs_separator))
        .saturating_add(key.len());
    if full_bytes <= limit {
        if needs_separator {
            return format!("{prefix}.{key}");
        }
        return key.to_owned();
    }
    if limit == 0 {
        return String::new();
    }

    let ellipsis = '…';
    let content_limit = limit.saturating_sub(ellipsis.len_utf8());
    let mut path = String::with_capacity(limit);
    push_bounded(&mut path, prefix, content_limit);
    if needs_separator && path.len() < content_limit {
        path.push('.');
    }
    push_bounded(&mut path, key, content_limit);
    if path.len() + ellipsis.len_utf8() <= limit {
        path.push(ellipsis);
    }
    path
}

fn push_bounded(output: &mut String, value: &str, limit: usize) {
    if output.len() >= limit {
        return;
    }
    let remaining = limit - output.len();
    let end = floor_char_boundary(value, remaining.min(value.len()));
    output.push_str(&value[..end]);
}

fn floor_char_boundary(value: &str, mut index: usize) -> usize {
    while !value.is_char_boundary(index) {
        index -= 1;
    }
    index
}

fn bounded_text(value: &str, limit: usize) -> String {
    if value.len() <= limit {
        return value.to_owned();
    }
    let ellipsis = '…';
    let content_limit = limit.saturating_sub(ellipsis.len_utf8());
    let end = floor_char_boundary(value, content_limit.min(value.len()));
    let mut output = String::with_capacity(limit);
    output.push_str(&value[..end]);
    if output.len() + ellipsis.len_utf8() <= limit {
        output.push(ellipsis);
    }
    output
}

fn apply_core(raw: &RawCore, settings: &mut Settings, problems: &mut Vec<ConfigProblem>) {
    if let Some(shell) = raw.shell.as_deref() {
        settings.core.shell = match shell {
            "" => None,
            "bash" => Some(Shell::Bash),
            "zsh" => Some(Shell::Zsh),
            "fish" => Some(Shell::Fish),
            _ => {
                problems.push(ConfigProblem::new(
                    "core.shell",
                    "expected empty, bash, zsh, or fish",
                ));
                None
            }
        };
    }
    if let Some(mode) = raw.mode.as_deref() {
        settings.core.mode = match mode {
            "last" => Mode::Last,
            "spec" => Mode::Spec,
            "history" => Mode::History,
            _ => {
                problems.push(ConfigProblem::new(
                    "core.mode",
                    "expected last, spec, or history",
                ));
                settings.core.mode
            }
        };
    }
    if let Some(debug) = raw.debug {
        settings.core.debug = debug;
    }
    if let Some(expand_alias) = raw.expand_alias {
        settings.core.expand_alias = expand_alias;
    }
    if let Some(infer_completions) = raw.infer_completions {
        settings.core.infer_completions = infer_completions;
    }
}

fn apply_ui(raw: &RawUi, settings: &mut Settings, problems: &mut Vec<ConfigProblem>) {
    if let Some(style) = raw.style.as_deref() {
        settings.ui.style = match style {
            "modern" => Style::Modern,
            "classic" => Style::Classic,
            _ => {
                problems.push(ConfigProblem::new("ui.style", "expected modern or classic"));
                settings.ui.style
            }
        };
    }
    if let Some(value) = raw.nerd_fonts {
        settings.ui.nerd_fonts = value;
    }
    if let Some(value) = raw.hidden_files {
        settings.ui.hidden_files = value;
    }
    if let Some(value) = raw.ghost_text {
        settings.ui.ghost_text = value;
    }
    if let Some(value) = raw.max_suggestions {
        settings.ui.max_suggestions = value;
    }
    if let Some(value) = raw.max_height {
        settings.ui.max_height = value;
    }
}

fn apply_keybindings(raw: &RawKeybindings, settings: &mut Settings) {
    if let Some(value) = &raw.toggle_mode {
        settings.keybindings.toggle_mode.clone_from(value);
    }
    if let Some(value) = &raw.toggle_menu {
        settings.keybindings.toggle_menu.clone_from(value);
    }
}

fn apply_git(raw: &RawGit, settings: &mut Settings) {
    if let Some(value) = raw.filter_active_branch {
        settings.git.filter_active_branch = value;
    }
    if let Some(value) = raw.deduplicate_branches {
        settings.git.deduplicate_branches = value;
    }
}

fn apply_updater(raw: &RawUpdater, settings: &mut Settings, problems: &mut Vec<ConfigProblem>) {
    if let Some(value) = raw.check_on_startup {
        settings.updater.check_on_startup = value;
    }
    if let Some(channel) = raw.channel.as_deref() {
        settings.updater.channel = match channel {
            "stable" => UpdateChannel::Stable,
            "nightly" => UpdateChannel::Nightly,
            _ => {
                problems.push(ConfigProblem::new(
                    "updater.channel",
                    "expected stable or nightly",
                ));
                settings.updater.channel
            }
        };
    }
    if let Some(interval) = raw.check_interval.as_deref() {
        match parse_duration(interval) {
            Ok(duration) => settings.updater.check_interval = duration,
            Err(message) => problems.push(ConfigProblem::new("updater.check-interval", message)),
        }
    }
}

fn apply_ai(raw: RawAi, settings: &mut Settings, problems: &mut Vec<ConfigProblem>) {
    if let Some(value) = raw.enabled {
        settings.ai.enabled = value;
    }
    if let Some(provider) = raw.provider {
        settings.ai.provider = (!provider.trim().is_empty()).then_some(provider);
    }
    if let Some(level) = raw.context_level.as_deref() {
        settings.ai.context_level = match level {
            "minimal" => AiContextLevel::Minimal,
            "workspace" => AiContextLevel::Workspace,
            "full" => AiContextLevel::Full,
            _ => {
                problems.push(ConfigProblem::new(
                    "ai.context-level",
                    "expected minimal, workspace, or full",
                ));
                settings.ai.context_level
            }
        };
    }
    if let Some(value) = raw.debounce_ms {
        settings.ai.debounce_ms = value;
    }
    if let Some(value) = raw.min_interval_ms {
        settings.ai.min_interval_ms = value;
    }

    settings.ai.providers = raw
        .providers
        .into_iter()
        .map(|(name, provider)| {
            let provider = convert_provider(&name, provider, problems);
            (name, provider)
        })
        .collect();
}

fn convert_provider(name: &str, raw: RawProvider, problems: &mut Vec<ConfigProblem>) -> AiProvider {
    let mut provider = AiProvider::default();
    if let Some(inherited_from) = raw.inherited_from.as_deref() {
        if inherited_from != "openai" {
            problems.push(ConfigProblem::new(
                format!("ai.providers.{name}.inherited_from"),
                "expected openai",
            ));
        }
        provider.inherited_from = ProviderProtocol::OpenAi;
    }
    provider.endpoint = raw.endpoint;
    provider.api_key_env = raw.api_key_env;
    provider.api_key = raw.api_key.map(Credential::new);
    provider.model = raw.model;
    if let Some(timeout_ms) = raw.timeout_ms {
        provider.timeout_ms = timeout_ms;
    }
    provider.extra_request_body = raw
        .extra_request_body
        .into_iter()
        .filter_map(|(key, value)| {
            let Some(value) = convert_extra_value(value, 0) else {
                problems.push(ConfigProblem::new(
                    format!("ai.providers.{name}.extra_request_body.{key}"),
                    "expected JSON-compatible TOML without datetime values",
                ));
                return None;
            };
            Some((key, value))
        })
        .collect();
    provider
}

fn convert_extra_value(value: toml::Value, depth: usize) -> Option<ExtraRequestValue> {
    if depth > 16 {
        return None;
    }
    match value {
        toml::Value::String(value) => Some(ExtraRequestValue::String(value)),
        toml::Value::Integer(value) => Some(ExtraRequestValue::Integer(value)),
        toml::Value::Float(value) if value.is_finite() => Some(ExtraRequestValue::Float(value)),
        toml::Value::Float(_) | toml::Value::Datetime(_) => None,
        toml::Value::Boolean(value) => Some(ExtraRequestValue::Boolean(value)),
        toml::Value::Array(values) => values
            .into_iter()
            .map(|value| convert_extra_value(value, depth + 1))
            .collect::<Option<Vec<_>>>()
            .map(ExtraRequestValue::Array),
        toml::Value::Table(values) => values
            .into_iter()
            .map(|(key, value)| convert_extra_value(value, depth + 1).map(|value| (key, value)))
            .collect::<Option<BTreeMap<_, _>>>()
            .map(ExtraRequestValue::Table),
    }
}

fn parse_duration(value: &str) -> Result<Duration, &'static str> {
    let digits = value.bytes().take_while(u8::is_ascii_digit).count();
    if digits == 0 || digits == value.len() {
        return Err("expected a positive duration such as 24h");
    }
    let amount = value[..digits]
        .parse::<u64>()
        .map_err(|_| "expected a positive duration such as 24h")?;
    if amount == 0 {
        return Err("expected a positive duration such as 24h");
    }
    let multiplier = match &value[digits..] {
        "s" => 1,
        "m" => 60,
        "h" => 60 * 60,
        "d" => 24 * 60 * 60,
        _ => return Err("expected a duration with s, m, h, or d suffix"),
    };
    amount
        .checked_mul(multiplier)
        .map(Duration::from_secs)
        .ok_or("expected a duration that fits in 64-bit seconds")
}

fn format_duration(duration: Duration) -> String {
    let seconds = duration.as_secs();
    for (unit, multiplier) in [("d", 86_400), ("h", 3_600), ("m", 60), ("s", 1)] {
        if seconds % multiplier == 0 {
            return format!("{}{unit}", seconds / multiplier);
        }
    }
    unreachable!("one-second unit divides every duration")
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct RawConfig {
    core: RawCore,
    ui: RawUi,
    keybindings: RawKeybindings,
    git: RawGit,
    updater: RawUpdater,
    ai: RawAi,
    #[serde(flatten)]
    unknown: BTreeMap<String, toml::Value>,
}

#[derive(Default, Deserialize)]
#[serde(default, rename_all = "kebab-case")]
struct RawCore {
    version: Option<u32>,
    shell: Option<String>,
    mode: Option<String>,
    debug: Option<bool>,
    #[serde(alias = "expand_alias")]
    expand_alias: Option<bool>,
    #[serde(alias = "infer_completions")]
    infer_completions: Option<bool>,
    #[serde(flatten)]
    unknown: BTreeMap<String, toml::Value>,
}

#[derive(Default, Deserialize)]
#[serde(default, rename_all = "kebab-case")]
struct RawUi {
    style: Option<String>,
    #[serde(alias = "nerd_fonts")]
    nerd_fonts: Option<bool>,
    #[serde(alias = "hidden_files")]
    hidden_files: Option<bool>,
    #[serde(alias = "ghost_text")]
    ghost_text: Option<bool>,
    #[serde(alias = "max_suggestions")]
    max_suggestions: Option<u16>,
    #[serde(alias = "max_height")]
    max_height: Option<u16>,
    #[serde(flatten)]
    unknown: BTreeMap<String, toml::Value>,
}

#[derive(Default, Deserialize)]
#[serde(default, rename_all = "kebab-case")]
struct RawKeybindings {
    #[serde(alias = "toggle_mode")]
    toggle_mode: Option<String>,
    #[serde(alias = "toggle_menu")]
    toggle_menu: Option<String>,
    #[serde(flatten)]
    unknown: BTreeMap<String, toml::Value>,
}

#[derive(Default, Deserialize)]
#[serde(default, rename_all = "kebab-case")]
struct RawGit {
    #[serde(alias = "filter_active_branch")]
    filter_active_branch: Option<bool>,
    #[serde(alias = "deduplicate_branches")]
    deduplicate_branches: Option<bool>,
    #[serde(flatten)]
    unknown: BTreeMap<String, toml::Value>,
}

#[derive(Default, Deserialize)]
#[serde(default, rename_all = "kebab-case")]
struct RawUpdater {
    #[serde(alias = "check_on_startup")]
    check_on_startup: Option<bool>,
    channel: Option<String>,
    #[serde(alias = "check_interval")]
    check_interval: Option<String>,
    #[serde(flatten)]
    unknown: BTreeMap<String, toml::Value>,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct RawAi {
    enabled: Option<bool>,
    provider: Option<String>,
    #[serde(rename = "context-level", alias = "context_level")]
    context_level: Option<String>,
    #[serde(alias = "debounce-ms")]
    debounce_ms: Option<u64>,
    #[serde(alias = "min-interval-ms")]
    min_interval_ms: Option<u64>,
    #[serde(alias = "suggest-on-empty")]
    suggest_on_empty: Option<bool>,
    providers: BTreeMap<String, RawProvider>,
    #[serde(flatten)]
    unknown: BTreeMap<String, toml::Value>,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct RawProvider {
    #[serde(alias = "inherited-from")]
    inherited_from: Option<String>,
    endpoint: Option<String>,
    #[serde(alias = "api-key-env")]
    api_key_env: Option<String>,
    #[serde(alias = "api-key")]
    api_key: Option<String>,
    model: Option<String>,
    #[serde(alias = "timeout-ms")]
    timeout_ms: Option<u64>,
    #[serde(alias = "extra-request-body")]
    extra_request_body: BTreeMap<String, toml::Value>,
    #[serde(flatten)]
    unknown: BTreeMap<String, toml::Value>,
}

#[derive(Serialize)]
struct OutputConfig<'a> {
    core: OutputCore<'a>,
    ui: OutputUi<'a>,
    keybindings: OutputKeybindings<'a>,
    git: OutputGit,
    updater: OutputUpdater<'a>,
    ai: OutputAi<'a>,
}

impl<'a> OutputConfig<'a> {
    fn new(settings: &'a Settings, redact: bool) -> Self {
        Self {
            core: OutputCore {
                version: CURRENT_SCHEMA_VERSION,
                shell: settings
                    .core
                    .shell
                    .map_or_else(String::new, |shell| shell.as_str().to_owned()),
                mode: match settings.core.mode {
                    Mode::Last => "last",
                    Mode::Spec => "spec",
                    Mode::History => "history",
                },
                debug: settings.core.debug,
                expand_alias: settings.core.expand_alias,
                infer_completions: settings.core.infer_completions,
            },
            ui: OutputUi {
                style: match settings.ui.style {
                    Style::Modern => "modern",
                    Style::Classic => "classic",
                },
                nerd_fonts: settings.ui.nerd_fonts,
                hidden_files: settings.ui.hidden_files,
                ghost_text: settings.ui.ghost_text,
                max_suggestions: settings.ui.max_suggestions,
                max_height: settings.ui.max_height,
            },
            keybindings: OutputKeybindings {
                toggle_mode: &settings.keybindings.toggle_mode,
                toggle_menu: &settings.keybindings.toggle_menu,
            },
            git: OutputGit {
                filter_active_branch: settings.git.filter_active_branch,
                deduplicate_branches: settings.git.deduplicate_branches,
            },
            updater: OutputUpdater {
                check_on_startup: settings.updater.check_on_startup,
                channel: match settings.updater.channel {
                    UpdateChannel::Stable => "stable",
                    UpdateChannel::Nightly => "nightly",
                },
                check_interval: format_duration(settings.updater.check_interval),
            },
            ai: OutputAi {
                enabled: settings.ai.enabled,
                provider: settings.ai.provider.as_deref().unwrap_or_default(),
                context_level: match settings.ai.context_level {
                    AiContextLevel::Minimal => "minimal",
                    AiContextLevel::Workspace => "workspace",
                    AiContextLevel::Full => "full",
                },
                debounce_ms: settings.ai.debounce_ms,
                min_interval_ms: settings.ai.min_interval_ms,
                providers: settings
                    .ai
                    .providers
                    .iter()
                    .map(|(name, provider)| (name, OutputProvider::new(provider, redact)))
                    .collect(),
            },
        }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "kebab-case")]
struct OutputCore<'a> {
    version: u32,
    shell: String,
    mode: &'a str,
    debug: bool,
    expand_alias: bool,
    infer_completions: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "kebab-case")]
struct OutputUi<'a> {
    style: &'a str,
    nerd_fonts: bool,
    hidden_files: bool,
    ghost_text: bool,
    max_suggestions: u16,
    max_height: u16,
}

#[derive(Serialize)]
#[serde(rename_all = "kebab-case")]
struct OutputKeybindings<'a> {
    toggle_mode: &'a str,
    toggle_menu: &'a str,
}

#[derive(Serialize)]
#[serde(rename_all = "kebab-case")]
struct OutputGit {
    filter_active_branch: bool,
    deduplicate_branches: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "kebab-case")]
struct OutputUpdater<'a> {
    check_on_startup: bool,
    channel: &'a str,
    check_interval: String,
}

#[derive(Serialize)]
struct OutputAi<'a> {
    enabled: bool,
    provider: &'a str,
    #[serde(rename = "context-level")]
    context_level: &'a str,
    debounce_ms: u64,
    min_interval_ms: u64,
    providers: BTreeMap<&'a String, OutputProvider<'a>>,
}

#[derive(Serialize)]
struct OutputProvider<'a> {
    inherited_from: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    endpoint: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    api_key_env: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    api_key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    model: Option<&'a str>,
    timeout_ms: u64,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    extra_request_body: BTreeMap<String, toml::Value>,
}

impl<'a> OutputProvider<'a> {
    fn new(provider: &'a AiProvider, redact: bool) -> Self {
        Self {
            inherited_from: match provider.inherited_from {
                ProviderProtocol::OpenAi => "openai",
            },
            endpoint: provider.endpoint.as_deref().map(|endpoint| {
                if redact {
                    redact_endpoint_for_display(endpoint)
                } else {
                    endpoint.to_owned()
                }
            }),
            api_key_env: provider.api_key_env.as_deref(),
            api_key: provider.api_key.as_ref().map(|credential| {
                if redact {
                    REDACTED_CREDENTIAL.to_owned()
                } else {
                    credential.expose_secret().to_owned()
                }
            }),
            model: provider.model.as_deref(),
            timeout_ms: provider.timeout_ms,
            extra_request_body: provider
                .extra_request_body
                .iter()
                .enumerate()
                .map(|(index, (key, value))| {
                    let output_key = if redact && is_secret_key(key) {
                        format!("redacted-field-{index}")
                    } else {
                        key.clone()
                    };
                    (output_key, extra_value_to_toml(key, value, redact))
                })
                .collect(),
        }
    }
}

fn extra_value_to_toml(key: &str, value: &ExtraRequestValue, redact: bool) -> toml::Value {
    if redact && is_secret_key(key) {
        return toml::Value::String(REDACTED_CREDENTIAL.to_owned());
    }
    match value {
        ExtraRequestValue::String(value) => toml::Value::String(value.clone()),
        ExtraRequestValue::Integer(value) => toml::Value::Integer(*value),
        ExtraRequestValue::Float(value) => toml::Value::Float(*value),
        ExtraRequestValue::Boolean(value) => toml::Value::Boolean(*value),
        ExtraRequestValue::Array(values) => toml::Value::Array(
            values
                .iter()
                .map(|value| extra_value_to_toml("", value, redact))
                .collect(),
        ),
        ExtraRequestValue::Table(values) => toml::Value::Table(
            values
                .iter()
                .enumerate()
                .map(|(index, (key, value))| {
                    let output_key = if redact && is_secret_key(key) {
                        format!("redacted-field-{index}")
                    } else {
                        key.clone()
                    };
                    (output_key, extra_value_to_toml(key, value, redact))
                })
                .collect(),
        ),
    }
}

fn is_secret_key(key: &str) -> bool {
    let normalized = key
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .flat_map(char::to_lowercase)
        .collect::<String>();
    normalized.contains("auth")
        || normalized.contains("credential")
        || normalized.contains("accesskey")
        || normalized.contains("apikey")
        || normalized == "key"
        || normalized.ends_with("key")
        || normalized.contains("password")
        || normalized.contains("secret")
        || normalized.contains("token")
}

fn redact_endpoint_for_display(endpoint: &str) -> String {
    let (without_query, has_query) = endpoint
        .split_once('?')
        .map_or((endpoint, false), |(base, _)| (base, true));
    let authority_start = without_query
        .find("://")
        .map_or(0, |scheme_end| scheme_end + 3);
    let authority_end = without_query[authority_start..]
        .find('/')
        .map_or(without_query.len(), |offset| authority_start + offset);
    let authority = &without_query[authority_start..authority_end];

    let mut display = if let Some(at) = authority.rfind('@') {
        let mut redacted = String::with_capacity(without_query.len());
        redacted.push_str(&without_query[..authority_start]);
        redacted.push_str("<redacted>@");
        redacted.push_str(&authority[at + 1..]);
        redacted.push_str(&without_query[authority_end..]);
        redacted
    } else {
        without_query.to_owned()
    };
    if has_query {
        display.push_str("?<redacted>");
    }
    display
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fmt::Write as _;

    #[test]
    fn defaults_and_current_template_parse() {
        let defaults = parse_config("").unwrap();
        assert_eq!(defaults.settings, Settings::default());
        assert_eq!(defaults.source_schema, 1);
        assert!(defaults.needs_migration());

        let template = parse_config(DEFAULT_CONFIG_TEMPLATE).unwrap();
        assert_eq!(template.settings, Settings::default());
        assert_eq!(template.source_schema, CURRENT_SCHEMA_VERSION);
        assert!(template.warnings.is_empty());
    }

    #[test]
    fn legacy_underscore_keys_migrate_without_changing_meaning() {
        let parsed = parse_config(
            r#"
                [core]
                version = 1
                expand_alias = false

                [ui]
                ghost_text = false
                max_suggestions = 42
                max_height = 9

                [ai]
                enabled = true
                provider = "greendale"
                context_level = "workspace"
                debounce_ms = 700
                min_interval_ms = 1500
                suggest_on_empty = true

                [ai.providers.greendale]
                inherited_from = "openai"
                endpoint = "https://example.invalid/v1"
                api_key_env = "GREENDALE_KEY"
                model = "dean-model"
                timeout_ms = 3000
            "#,
        )
        .unwrap();

        assert!(parsed.needs_migration());
        assert!(!parsed.settings.core.expand_alias);
        assert!(!parsed.settings.ui.ghost_text);
        assert_eq!(parsed.settings.ui.max_suggestions, 42);
        assert_eq!(parsed.settings.ai.context_level, AiContextLevel::Workspace);
        assert_eq!(parsed.settings.ai.debounce_ms, 700);
        assert_eq!(parsed.settings.ai.min_interval_ms, 1500);
        assert_eq!(parsed.warnings[0].path, "ai.suggest_on_empty");
    }

    #[test]
    fn unknown_keys_warn_without_retaining_values() {
        let parsed = parse_config(
            r#"
                top-secret = "Troy's password"
                [ui]
                mystery = "Annie's token"
                [ai.providers.greendale]
                surprise = "Dean's API key"
            "#,
        )
        .unwrap();

        assert_eq!(
            parsed
                .warnings
                .iter()
                .map(|warning| warning.path.as_str())
                .collect::<Vec<_>>(),
            [
                "top-secret",
                "ui.mystery",
                "ai.providers.greendale.surprise"
            ]
        );
        let debug = format!("{parsed:?}");
        assert!(!debug.contains("password"));
        assert!(!debug.contains("Annie's token"));
        assert!(!debug.contains("API key"));
    }

    #[test]
    fn unknown_key_warnings_are_bounded_and_report_truncation() {
        let provider = "p".repeat(4_096);
        let mut input = format!("[ai.providers.{provider}]\n");
        for index in 0..300 {
            writeln!(input, "unknown_{index:03} = 'Dean secret'").unwrap();
        }

        let parsed = parse_config(&input).unwrap();
        assert!(parsed.warnings.len() <= MAX_CONFIG_WARNINGS);
        assert!(parsed.warnings.len() > 1);
        assert_eq!(
            parsed.warnings.last().unwrap(),
            &ConfigWarning {
                path: TRUNCATED_WARNING_PATH.to_owned(),
                reason: "additional unknown keys omitted",
            }
        );
        assert!(
            parsed
                .warnings
                .iter()
                .all(|warning| warning.path.len() <= MAX_CONFIG_WARNING_PATH_BYTES)
        );
        assert!(
            parsed
                .warnings
                .iter()
                .map(|warning| warning.path.len())
                .sum::<usize>()
                <= MAX_CONFIG_WARNING_BYTES
        );
        let debug = format!("{parsed:?}");
        assert!(debug.len() < 100_000);
        assert!(!debug.contains("Dean secret"));

        let mut many_short_keys = String::new();
        for index in 0..300 {
            writeln!(many_short_keys, "unknown_{index:03} = true").unwrap();
        }
        let parsed = parse_config(&many_short_keys).unwrap();
        assert_eq!(parsed.warnings.len(), MAX_CONFIG_WARNINGS);
        assert_eq!(
            parsed.warnings.last().unwrap().reason,
            "additional unknown keys omitted"
        );
    }

    #[test]
    fn invalid_values_are_aggregated_without_input_echo() {
        let error = parse_config(
            r#"
                [core]
                version = 99
                shell = "hunter2"
                mode = "password"
                [ui]
                style = "secret"
                max-height = 2
                [updater]
                channel = "token"
                check-interval = "forever"
            "#,
        )
        .unwrap_err();

        let ConfigFileError::Invalid(problems) = error else {
            panic!("wanted invalid fields")
        };
        assert!(problems.problems().len() >= 6);
        let display = problems.to_string();
        assert!(display.contains("core.version"));
        assert!(!display.contains("hunter2"));
        assert!(!display.contains("password"));
        assert!(!display.contains("secret"));
        assert!(!display.contains("token"));
    }

    #[test]
    fn unknown_selected_provider_is_an_operational_not_global_failure() {
        let secret = "Troy-private-provider-token";
        let document =
            parse_config(&format!("[ai]\nenabled = true\nprovider = {secret:?}\n")).unwrap();
        let error = document.settings.ai.readiness().unwrap_err();
        let rendered = format!("{error:?} {error}");
        assert!(!rendered.contains(secret));
        assert!(rendered.contains("UnknownProvider"));
        assert!(rendered.contains("no matching provider configuration"));
    }

    #[test]
    fn resolved_output_redacts_credentials_and_nested_secret_fields() {
        let mut settings = Settings::default();
        settings.ai.providers.insert(
            "greendale".to_owned(),
            AiProvider {
                endpoint: Some("https://example.invalid/v1".to_owned()),
                api_key_env: Some("GREENDALE_KEY".to_owned()),
                api_key: Some(Credential::new("study-room-secret")),
                model: Some("dean-model".to_owned()),
                extra_request_body: BTreeMap::from([
                    (
                        "authorization".to_owned(),
                        ExtraRequestValue::String("Bearer secret".to_owned()),
                    ),
                    (
                        "apiKey".to_owned(),
                        ExtraRequestValue::String("camel-case-api-key".to_owned()),
                    ),
                    (
                        "accessKey".to_owned(),
                        ExtraRequestValue::String("camel-case-access-key".to_owned()),
                    ),
                    (
                        "nested".to_owned(),
                        ExtraRequestValue::Table(BTreeMap::from([(
                            "access-token".to_owned(),
                            ExtraRequestValue::String("another secret".to_owned()),
                        )])),
                    ),
                ]),
                ..AiProvider::default()
            },
        );

        let output = render_resolved_config(&settings).unwrap();
        assert!(output.contains(REDACTED_CREDENTIAL));
        assert!(!output.contains("study-room-secret"));
        assert!(!output.contains("Bearer secret"));
        assert!(!output.contains("another secret"));
        assert!(!output.contains("camel-case-api-key"));
        assert!(!output.contains("camel-case-access-key"));
        assert!(!output.contains("apiKey"));
        assert!(!output.contains("accessKey"));

        let reparsed = parse_config(&output).unwrap();
        assert_eq!(reparsed.source_schema, CURRENT_SCHEMA_VERSION);
    }

    #[test]
    fn resolved_output_redacts_endpoint_userinfo_and_query() {
        let mut settings = Settings::default();
        settings.ai.providers.insert(
            "greendale".to_owned(),
            AiProvider {
                endpoint: Some("https://troy:secret@example.invalid/v1?api_key=hunter2".to_owned()),
                ..AiProvider::default()
            },
        );

        let output = render_resolved_config(&settings).unwrap();
        assert!(output.contains("https://<redacted>@example.invalid/v1?<redacted>"));
        assert!(!output.contains("troy"));
        assert!(!output.contains("secret"));
        assert!(!output.contains("hunter2"));
    }

    #[test]
    fn limits_and_incompatible_toml_fail_safely() {
        assert_eq!(
            parse_config(&"x".repeat(MAX_CONFIG_BYTES + 1)),
            Err(ConfigFileError::TooLarge {
                bytes: MAX_CONFIG_BYTES + 1
            })
        );
        assert_eq!(
            parse_config("[core\npassword = 'hunter2'"),
            Err(ConfigFileError::InvalidToml)
        );
        assert_eq!(
            parse_config("[ai]\ndebounce_ms = 'hunter2'"),
            Err(ConfigFileError::InvalidToml)
        );
    }

    #[test]
    fn extra_body_rejects_datetime_and_excessive_depth() {
        let datetime = parse_config(
            r"
                [ai.providers.greendale.extra_request_body]
                created = 1979-09-22
            ",
        )
        .unwrap_err();
        assert!(datetime.to_string().contains("extra_request_body.<field>"));

        let mut nested = String::from("[ai.providers.greendale.extra_request_body]\nvalue = ");
        nested.push_str(&"[".repeat(18));
        nested.push('1');
        nested.push_str(&"]".repeat(18));
        let depth = parse_config(&nested).unwrap_err();
        assert!(depth.to_string().contains("extra_request_body.<field>"));
    }
}
