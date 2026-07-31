//! Typed CLI and environment precedence over parsed configuration.

use std::error::Error;
use std::fmt;
use std::time::Duration;

use super::{Mode, Settings, Shell, UpdateChannel, ValidationErrors};

/// Documented environment variable for core debug logging.
pub const ENV_CORE_DEBUG: &str = "argmax_CORE_DEBUG";
/// Documented environment variable for shell selection.
pub const ENV_CORE_SHELL: &str = "argmax_CORE_SHELL";
/// Documented environment variable for initial completion mode.
pub const ENV_CORE_MODE: &str = "argmax_CORE_MODE";
/// Documented environment variable for ghost text.
pub const ENV_UI_GHOST_TEXT: &str = "argmax_UI_GHOST_TEXT";
/// Documented environment variable for the ranked result limit.
pub const ENV_UI_MAX_SUGGESTIONS: &str = "argmax_UI_MAX_SUGGESTIONS";
/// Documented environment variable for the visible menu height.
pub const ENV_UI_MAX_HEIGHT: &str = "argmax_UI_MAX_HEIGHT";
/// Documented environment variable for the update channel.
pub const ENV_UPDATER_CHANNEL: &str = "argmax_UPDATER_CHANNEL";
/// Documented environment variable for the update interval.
pub const ENV_UPDATER_INTERVAL: &str = "argmax_UPDATER_INTERVAL";
/// Documented environment variable for startup update checks.
pub const ENV_UPDATER_CHECK_ON_STARTUP: &str = "argmax_UPDATER_CHECK_ON_STARTUP";
/// Documented environment variable for AI enablement.
pub const ENV_AI_ENABLED: &str = "argmax_AI_ENABLED";
/// Documented environment variable for the selected AI provider.
pub const ENV_AI_PROVIDER: &str = "argmax_AI_PROVIDER";

/// Typed values read from only the documented environment overrides.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct EnvironmentOverrides {
    /// Core debug override.
    pub core_debug: Option<bool>,
    /// Core shell override.
    pub core_shell: Option<Shell>,
    /// Core mode override.
    pub core_mode: Option<Mode>,
    /// Ghost-text override.
    pub ui_ghost_text: Option<bool>,
    /// Ranked result-limit override.
    pub ui_max_suggestions: Option<u16>,
    /// Visible menu-height override.
    pub ui_max_height: Option<u16>,
    /// Update-channel override.
    pub updater_channel: Option<UpdateChannel>,
    /// Update-interval override.
    pub updater_interval: Option<Duration>,
    /// Startup update-check override.
    pub updater_check_on_startup: Option<bool>,
    /// AI enablement override.
    pub ai_enabled: Option<bool>,
    /// Selected AI-provider override.
    pub ai_provider: Option<String>,
}

impl EnvironmentOverrides {
    /// Reads and parses only supported variables through a caller-supplied lookup.
    ///
    /// This does not mutate process state or a parsed [`Settings`] value. All
    /// malformed present values are returned together.
    ///
    /// # Errors
    ///
    /// Returns one [`OverrideError`] for every present value that cannot be
    /// parsed as its documented type.
    pub fn from_lookup(
        mut lookup: impl FnMut(&str) -> Option<String>,
    ) -> Result<Self, OverrideErrors> {
        let mut errors = Vec::new();
        let overrides = Self {
            core_debug: read_override(&mut lookup, ENV_CORE_DEBUG, parse_boolean, &mut errors),
            core_shell: read_override(&mut lookup, ENV_CORE_SHELL, parse_shell, &mut errors),
            core_mode: read_override(&mut lookup, ENV_CORE_MODE, parse_mode, &mut errors),
            ui_ghost_text: read_override(
                &mut lookup,
                ENV_UI_GHOST_TEXT,
                parse_boolean,
                &mut errors,
            ),
            ui_max_suggestions: read_override(
                &mut lookup,
                ENV_UI_MAX_SUGGESTIONS,
                parse_u16,
                &mut errors,
            ),
            ui_max_height: read_override(&mut lookup, ENV_UI_MAX_HEIGHT, parse_u16, &mut errors),
            updater_channel: read_override(
                &mut lookup,
                ENV_UPDATER_CHANNEL,
                parse_update_channel,
                &mut errors,
            ),
            updater_interval: read_override(
                &mut lookup,
                ENV_UPDATER_INTERVAL,
                parse_duration,
                &mut errors,
            ),
            updater_check_on_startup: read_override(
                &mut lookup,
                ENV_UPDATER_CHECK_ON_STARTUP,
                parse_boolean,
                &mut errors,
            ),
            ai_enabled: read_override(&mut lookup, ENV_AI_ENABLED, parse_boolean, &mut errors),
            ai_provider: read_override(&mut lookup, ENV_AI_PROVIDER, parse_nonblank, &mut errors),
        };

        if errors.is_empty() {
            Ok(overrides)
        } else {
            Err(OverrideErrors { errors })
        }
    }

    fn apply_to(&self, settings: &mut Settings) {
        if let Some(value) = self.core_debug {
            settings.core.debug = value;
        }
        if let Some(value) = self.core_shell {
            settings.core.shell = Some(value);
        }
        if let Some(value) = self.core_mode {
            settings.core.mode = value;
        }
        if let Some(value) = self.ui_ghost_text {
            settings.ui.ghost_text = value;
        }
        if let Some(value) = self.ui_max_suggestions {
            settings.ui.max_suggestions = value;
        }
        if let Some(value) = self.ui_max_height {
            settings.ui.max_height = value;
        }
        if let Some(value) = self.updater_channel {
            settings.updater.channel = value;
        }
        if let Some(value) = self.updater_interval {
            settings.updater.check_interval = value;
        }
        if let Some(value) = self.updater_check_on_startup {
            settings.updater.check_on_startup = value;
        }
        if let Some(value) = self.ai_enabled {
            settings.ai.enabled = value;
        }
        if let Some(value) = &self.ai_provider {
            settings.ai.provider = Some(value.clone());
        }
    }
}

/// Session-only flags with precedence over environment and file settings.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct CliOverrides {
    /// `--shell`, when supplied.
    pub shell: Option<Shell>,
    /// `--debug`, represented as `Some(true)` when supplied.
    pub debug: Option<bool>,
}

impl CliOverrides {
    fn apply_to(self, settings: &mut Settings) {
        if let Some(shell) = self.shell {
            settings.core.shell = Some(shell);
        }
        if let Some(debug) = self.debug {
            settings.core.debug = debug;
        }
    }
}

/// Applies default/file, environment, then CLI precedence and validates once.
///
/// The parsed file value is cloned before overrides are applied, ensuring
/// process-local overrides cannot be persisted by a caller retaining the source.
///
/// # Errors
///
/// Returns aggregate resolved-setting validation failures after all precedence
/// layers have been applied.
pub fn resolve_settings(
    file: Option<&Settings>,
    environment: &EnvironmentOverrides,
    cli: CliOverrides,
) -> Result<Settings, ValidationErrors> {
    let mut resolved = file.cloned().unwrap_or_default();
    environment.apply_to(&mut resolved);
    cli.apply_to(&mut resolved);
    resolved.validate()?;
    Ok(resolved)
}

/// One malformed documented environment override.
///
/// The present value is never retained or displayed: pasted secrets land in
/// environment variables often enough that echoing one back is a leak.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OverrideError {
    /// Exact environment variable name.
    pub variable: &'static str,
    /// Stable expected-value description.
    pub expected: &'static str,
}

impl fmt::Display for OverrideError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{} has an unusable value: expected {}",
            self.variable, self.expected
        )
    }
}

/// Aggregate malformed environment overrides.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OverrideErrors {
    errors: Vec<OverrideError>,
}

impl OverrideErrors {
    /// Individual malformed overrides in documented variable order.
    #[must_use]
    pub fn errors(&self) -> &[OverrideError] {
        &self.errors
    }
}

impl fmt::Display for OverrideErrors {
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

impl Error for OverrideErrors {}

fn read_override<T>(
    lookup: &mut impl FnMut(&str) -> Option<String>,
    variable: &'static str,
    parse: fn(&str) -> Result<T, &'static str>,
    errors: &mut Vec<OverrideError>,
) -> Option<T> {
    let value = lookup(variable)?;
    match parse(&value) {
        Ok(parsed) => Some(parsed),
        Err(expected) => {
            errors.push(OverrideError { variable, expected });
            None
        }
    }
}

fn parse_boolean(value: &str) -> Result<bool, &'static str> {
    match value {
        "true" | "1" => Ok(true),
        "false" | "0" => Ok(false),
        _ => Err("true, false, 1, or 0"),
    }
}

fn parse_shell(value: &str) -> Result<Shell, &'static str> {
    match value {
        "bash" => Ok(Shell::Bash),
        "zsh" => Ok(Shell::Zsh),
        "fish" => Ok(Shell::Fish),
        _ => Err("bash, zsh, or fish"),
    }
}

fn parse_mode(value: &str) -> Result<Mode, &'static str> {
    match value {
        "last" => Ok(Mode::Last),
        "spec" => Ok(Mode::Spec),
        "history" => Ok(Mode::History),
        _ => Err("last, spec, or history"),
    }
}

fn parse_update_channel(value: &str) -> Result<UpdateChannel, &'static str> {
    match value {
        "stable" => Ok(UpdateChannel::Stable),
        "nightly" => Ok(UpdateChannel::Nightly),
        _ => Err("stable or nightly"),
    }
}

fn parse_u16(value: &str) -> Result<u16, &'static str> {
    value.parse().map_err(|_| "an unsigned integer")
}

fn parse_nonblank(value: &str) -> Result<String, &'static str> {
    if value.trim().is_empty() {
        Err("a non-blank provider name")
    } else {
        Ok(value.to_owned())
    }
}

fn parse_duration(value: &str) -> Result<Duration, &'static str> {
    let digits = value.bytes().take_while(u8::is_ascii_digit).count();
    if digits == 0 || digits == value.len() {
        return Err("a positive duration such as 24h");
    }
    let amount = value[..digits]
        .parse::<u64>()
        .map_err(|_| "a positive duration such as 24h")?;
    if amount == 0 {
        return Err("a positive duration such as 24h");
    }
    let multiplier = match &value[digits..] {
        "s" => 1,
        "m" => 60,
        "h" => 60 * 60,
        "d" => 24 * 60 * 60,
        _ => return Err("a duration with s, m, h, or d suffix"),
    };
    let seconds = amount
        .checked_mul(multiplier)
        .ok_or("a duration that fits in 64-bit seconds")?;
    Ok(Duration::from_secs(seconds))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;

    fn environment(values: &[(&str, &str)]) -> Result<EnvironmentOverrides, OverrideErrors> {
        EnvironmentOverrides::from_lookup(|name| {
            values
                .iter()
                .find(|(candidate, _)| *candidate == name)
                .map(|(_, value)| (*value).to_owned())
        })
    }

    #[test]
    fn documented_environment_values_apply_without_mutating_the_file() {
        let mut file = Settings::default();
        file.core.debug = false;
        file.ui.max_height = 7;
        let original = file.clone();
        let overrides = environment(&[
            (ENV_CORE_DEBUG, "true"),
            (ENV_CORE_SHELL, "fish"),
            (ENV_CORE_MODE, "history"),
            (ENV_UI_GHOST_TEXT, "false"),
            (ENV_UI_MAX_SUGGESTIONS, "42"),
            (ENV_UI_MAX_HEIGHT, "12"),
            (ENV_UPDATER_CHANNEL, "nightly"),
            (ENV_UPDATER_INTERVAL, "2h"),
            (ENV_UPDATER_CHECK_ON_STARTUP, "0"),
            (ENV_AI_ENABLED, "0"),
            (ENV_AI_PROVIDER, "greendale"),
        ])
        .unwrap();

        let resolved = resolve_settings(Some(&file), &overrides, CliOverrides::default()).unwrap();

        assert_eq!(file, original);
        assert!(resolved.core.debug);
        assert_eq!(resolved.core.shell, Some(Shell::Fish));
        assert_eq!(resolved.core.mode, Mode::History);
        assert!(!resolved.ui.ghost_text);
        assert_eq!(resolved.ui.max_suggestions, 42);
        assert_eq!(resolved.ui.max_height, 12);
        assert_eq!(resolved.updater.channel, UpdateChannel::Nightly);
        assert_eq!(resolved.updater.check_interval, Duration::from_secs(7_200));
        assert!(!resolved.updater.check_on_startup);
        assert!(!resolved.ai.enabled);
        assert_eq!(resolved.ai.provider.as_deref(), Some("greendale"));
    }

    #[test]
    fn cli_values_have_highest_precedence() {
        let mut file = Settings::default();
        file.core.shell = Some(Shell::Bash);
        let environment = EnvironmentOverrides {
            core_debug: Some(false),
            core_shell: Some(Shell::Fish),
            ..EnvironmentOverrides::default()
        };
        let cli = CliOverrides {
            shell: Some(Shell::Zsh),
            debug: Some(true),
        };

        let resolved = resolve_settings(Some(&file), &environment, cli).unwrap();

        assert_eq!(resolved.core.shell, Some(Shell::Zsh));
        assert!(resolved.core.debug);
    }

    #[test]
    fn malformed_overrides_are_aggregated_in_documented_order() {
        let cases = BTreeMap::from([
            (ENV_AI_ENABLED, "sometimes"),
            (ENV_AI_PROVIDER, " "),
            (ENV_CORE_DEBUG, "yes"),
            (ENV_CORE_MODE, "menu"),
            (ENV_CORE_SHELL, "/bin/zsh"),
            (ENV_UI_MAX_HEIGHT, "-1"),
            (ENV_UPDATER_INTERVAL, "forever"),
        ]);
        let errors = EnvironmentOverrides::from_lookup(|name| {
            cases.get(name).map(|value| (*value).to_owned())
        })
        .unwrap_err();

        assert_eq!(errors.errors().len(), cases.len());
        assert_eq!(errors.errors()[0].variable, ENV_CORE_DEBUG);
        assert_eq!(errors.errors()[1].variable, ENV_CORE_SHELL);
        assert_eq!(errors.errors().last().unwrap().variable, ENV_AI_PROVIDER);
        assert!(errors.to_string().contains("expected"));
    }

    #[test]
    fn parsed_but_out_of_range_values_fail_resolved_validation() {
        let overrides =
            environment(&[(ENV_UI_MAX_SUGGESTIONS, "0"), (ENV_UPDATER_INTERVAL, "31d")]).unwrap();
        let errors = resolve_settings(None, &overrides, CliOverrides::default()).unwrap_err();

        assert!(
            errors
                .errors()
                .iter()
                .any(|error| error.field == "ui.max-suggestions")
        );
        assert!(
            errors
                .errors()
                .iter()
                .any(|error| error.field == "updater.check-interval")
        );
    }

    #[test]
    fn duration_units_and_overflow_are_explicit() {
        let cases = BTreeMap::from([
            ("day", ("1d", Some(86_400))),
            ("hour", ("2h", Some(7_200))),
            ("minute", ("3m", Some(180))),
            ("missing suffix", ("24", None)),
            ("overflow", ("18446744073709551615d", None)),
            ("second", ("4s", Some(4))),
            ("zero", ("0h", None)),
        ]);

        for (name, (value, want_seconds)) in cases {
            assert_eq!(
                parse_duration(value)
                    .ok()
                    .map(|duration| duration.as_secs()),
                want_seconds,
                "{name}"
            );
        }
    }
}
