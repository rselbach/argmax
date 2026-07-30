//! Bounded contracts for Cobra's hidden completion protocol.
//!
//! This module only validates requests and parses captured standard output. It
//! deliberately does not resolve or execute programs.

use std::error::Error;
use std::fmt;
use std::path::PathBuf;
use std::time::{Duration, SystemTime};

/// Hard wall-clock budget for a Cobra completion process.
pub const COBRA_COMPLETION_TIMEOUT: Duration = Duration::from_millis(300);
/// Largest standard-output body accepted from a Cobra completion process.
pub const MAX_COBRA_OUTPUT_BYTES: usize = 256 * 1024;
/// Largest number of completion records accepted from one Cobra response.
pub const MAX_COBRA_CANDIDATES: usize = 500;

/// Cobra's directive bit indicating that completion failed.
pub const DIRECTIVE_ERROR: u32 = 1;
/// Cobra's directive bit suppressing a trailing space after insertion.
pub const DIRECTIVE_NO_SPACE: u32 = 1 << 1;
/// Cobra's directive bit suppressing fallback filesystem completion.
pub const DIRECTIVE_NO_FILE_COMPLETION: u32 = 1 << 2;
/// Cobra's directive bit interpreting response values as file extensions.
pub const DIRECTIVE_FILTER_FILE_EXTENSIONS: u32 = 1 << 3;
/// Cobra's directive bit requesting directory-only filesystem completion.
pub const DIRECTIVE_FILTER_DIRECTORIES: u32 = 1 << 4;
/// Cobra's directive bit preserving the response's candidate order.
pub const DIRECTIVE_KEEP_ORDER: u32 = 1 << 5;
/// Mask containing every Cobra directive understood by argmax.
pub const KNOWN_DIRECTIVE_BITS: u32 = DIRECTIVE_ERROR
    | DIRECTIVE_NO_SPACE
    | DIRECTIVE_NO_FILE_COMPLETION
    | DIRECTIVE_FILTER_FILE_EXTENSIONS
    | DIRECTIVE_FILTER_DIRECTORIES
    | DIRECTIVE_KEEP_ORDER;

const COBRA_COMPLETE_ARGUMENT: &str = "__complete";

/// A validated executable basename suitable for direct, shell-free lookup.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct CobraExecutable(String);

impl CobraExecutable {
    /// Validates an executable basename.
    ///
    /// Names containing either Unix or Windows path separators, control text,
    /// or the special `.` and `..` path components are rejected.
    ///
    /// # Errors
    ///
    /// Returns [`CobraProtocolError::InvalidExecutableBasename`] when `name`
    /// is not a safe basename.
    pub fn new(name: impl Into<String>) -> Result<Self, CobraProtocolError> {
        let name = name.into();
        if name.is_empty()
            || matches!(name.as_str(), "." | "..")
            || name.contains(['/', '\\'])
            || name.chars().any(char::is_control)
        {
            return Err(CobraProtocolError::InvalidExecutableBasename);
        }
        Ok(Self(name))
    }

    /// Returns the validated executable basename.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// One structured request for `<executable> __complete <args> <partial>`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CobraRequest {
    executable: CobraExecutable,
    committed_arguments: Vec<String>,
    partial: String,
}

impl CobraRequest {
    /// Builds a request without joining or shell-interpreting its arguments.
    ///
    /// `committed_arguments` excludes the executable and active partial token.
    /// The partial token is always emitted as the final argument, including
    /// when it is empty.
    ///
    /// # Errors
    ///
    /// Returns an error for an unsafe executable basename or an argument that
    /// contains a NUL byte and therefore cannot be passed to a process API.
    pub fn new<I, S>(
        executable: impl Into<String>,
        committed_arguments: I,
        partial: impl Into<String>,
    ) -> Result<Self, CobraProtocolError>
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let executable = CobraExecutable::new(executable)?;
        let committed_arguments = committed_arguments
            .into_iter()
            .map(Into::into)
            .collect::<Vec<_>>();
        let partial = partial.into();

        if committed_arguments
            .iter()
            .chain(std::iter::once(&partial))
            .any(|argument| argument.contains('\0'))
        {
            return Err(CobraProtocolError::InvalidArgument);
        }

        Ok(Self {
            executable,
            committed_arguments,
            partial,
        })
    }

    /// Returns the validated executable basename passed to the process API.
    #[must_use]
    pub const fn executable(&self) -> &CobraExecutable {
        &self.executable
    }

    /// Returns committed command arguments, excluding the active partial.
    #[must_use]
    pub fn committed_arguments(&self) -> &[String] {
        &self.committed_arguments
    }

    /// Returns the active partial argument.
    #[must_use]
    pub fn partial(&self) -> &str {
        &self.partial
    }

    /// Constructs the exact argument vector for a direct process invocation.
    #[must_use]
    pub fn argv(&self) -> Vec<String> {
        let mut argv = Vec::with_capacity(self.committed_arguments.len() + 2);
        argv.push(COBRA_COMPLETE_ARGUMENT.to_owned());
        argv.extend(self.committed_arguments.iter().cloned());
        argv.push(self.partial.clone());
        argv
    }

    /// Creates a successful-result cache key for a resolved binary.
    ///
    /// Callers should insert only successfully parsed results under this key;
    /// transient resolution, execution, timeout, and parse failures are not
    /// cacheable protocol results.
    #[must_use]
    pub fn success_cache_key(
        &self,
        binary: CobraBinaryIdentity,
        working_directory: impl Into<PathBuf>,
    ) -> CobraCacheKey {
        CobraCacheKey {
            binary,
            working_directory: working_directory.into(),
            committed_arguments: self.committed_arguments.clone(),
            partial: self.partial.clone(),
        }
    }
}

/// Resolved identity used to invalidate cached Cobra responses after replacement.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct CobraBinaryIdentity {
    /// Resolved executable path rather than its requested basename.
    pub resolved_path: PathBuf,
    /// Modification timestamp observed for the resolved executable.
    pub modified: SystemTime,
}

impl CobraBinaryIdentity {
    /// Creates a binary identity from lookup and metadata results.
    #[must_use]
    pub fn new(resolved_path: impl Into<PathBuf>, modified: SystemTime) -> Self {
        Self {
            resolved_path: resolved_path.into(),
            modified,
        }
    }
}

/// Complete identity for one successfully parsed Cobra response.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct CobraCacheKey {
    /// Resolved executable identity and modification timestamp.
    pub binary: CobraBinaryIdentity,
    /// Exact absolute working directory in which completion executes.
    pub working_directory: PathBuf,
    /// Committed arguments preceding the active partial token.
    pub committed_arguments: Vec<String>,
    /// Active partial token sent as the final protocol argument.
    pub partial: String,
}

/// Validated Cobra directive bitset.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CobraDirective(u32);

impl CobraDirective {
    /// Returns the raw, validated Cobra directive bits.
    #[must_use]
    pub const fn bits(self) -> u32 {
        self.0
    }

    /// Returns whether insertion must omit its usual trailing space.
    #[must_use]
    pub const fn no_space(self) -> bool {
        self.contains(DIRECTIVE_NO_SPACE)
    }

    /// Returns whether filesystem fallback is explicitly disabled.
    #[must_use]
    pub const fn no_file_completion(self) -> bool {
        self.contains(DIRECTIVE_NO_FILE_COMPLETION)
    }

    /// Returns whether the source candidate order must be preserved.
    #[must_use]
    pub const fn keep_order(self) -> bool {
        self.contains(DIRECTIVE_KEEP_ORDER)
    }

    const fn contains(self, bit: u32) -> bool {
        self.0 & bit != 0
    }
}

/// One safe candidate decoded from a Cobra response.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CobraCandidate {
    /// Argument value offered for insertion.
    pub value: String,
    /// Optional short description emitted after a tab.
    pub description: String,
}

/// Filesystem behavior requested by a successful Cobra response.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CobraFileCompletion {
    /// Use ordinary filesystem fallback if no protocol candidate applies.
    Default,
    /// Do not perform fallback filesystem completion.
    Disabled,
    /// Treat response values as allowed file extensions, not suggestions.
    FilterExtensions(Vec<String>),
    /// Complete directories, optionally relative to the returned directory.
    FilterDirectories {
        /// Directory in which completion should begin, or the current directory.
        within: Option<String>,
    },
}

/// A fully validated, bounded Cobra completion response.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CobraCompletion {
    /// Ordinary candidates; empty for filesystem-filter directives.
    pub candidates: Vec<CobraCandidate>,
    /// Validated raw directive metadata.
    pub directive: CobraDirective,
    /// Explicit filesystem fallback or filtering behavior.
    pub file_completion: CobraFileCompletion,
}

/// Failure to construct or parse a Cobra protocol exchange.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CobraProtocolError {
    /// The requested program was not a safe executable basename.
    InvalidExecutableBasename,
    /// One structured process argument contained an unsupported NUL byte.
    InvalidArgument,
    /// Captured standard output exceeded the hard byte limit.
    OutputTooLarge {
        /// Observed output size in bytes.
        size: usize,
        /// Maximum accepted output size in bytes.
        limit: usize,
    },
    /// Captured standard output was not valid UTF-8.
    InvalidUtf8,
    /// The final protocol directive line was absent.
    MissingDirective,
    /// The final protocol directive was not exactly `:<unsigned decimal>`.
    MalformedDirective,
    /// Cobra's error directive was set; all candidates must be ignored.
    ErrorDirective,
    /// The response set directive bits this version does not understand.
    UnknownDirectiveBits(u32),
    /// Both mutually exclusive filesystem-filter directives were set.
    ConflictingFileDirectives,
    /// A candidate line had no value or otherwise violated protocol framing.
    MalformedCandidate {
        /// One-based output line number.
        line: usize,
    },
    /// A candidate value or description contained terminal control text.
    ControlText {
        /// One-based output line number.
        line: usize,
    },
    /// The response exceeded the hard candidate-count limit.
    TooManyCandidates {
        /// Maximum accepted candidate count.
        limit: usize,
    },
    /// Directory filtering named more than one base directory.
    TooManyDirectoryFilters,
}

impl fmt::Display for CobraProtocolError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidExecutableBasename => formatter.write_str("invalid executable basename"),
            Self::InvalidArgument => formatter.write_str("Cobra argument contains a NUL byte"),
            Self::OutputTooLarge { size, limit } => {
                write!(formatter, "Cobra output is {size} bytes; limit is {limit}")
            }
            Self::InvalidUtf8 => formatter.write_str("Cobra output is not valid UTF-8"),
            Self::MissingDirective => formatter.write_str("Cobra directive line is missing"),
            Self::MalformedDirective => formatter.write_str("malformed Cobra directive line"),
            Self::ErrorDirective => formatter.write_str("Cobra reported a completion error"),
            Self::UnknownDirectiveBits(bits) => {
                write!(formatter, "unknown Cobra directive bits: {bits}")
            }
            Self::ConflictingFileDirectives => {
                formatter.write_str("conflicting Cobra filesystem directives")
            }
            Self::MalformedCandidate { line } => {
                write!(formatter, "malformed Cobra candidate on line {line}")
            }
            Self::ControlText { line } => {
                write!(formatter, "control text in Cobra candidate on line {line}")
            }
            Self::TooManyCandidates { limit } => {
                write!(formatter, "Cobra response exceeds {limit} candidates")
            }
            Self::TooManyDirectoryFilters => {
                formatter.write_str("Cobra directory filter has multiple base directories")
            }
        }
    }
}

impl Error for CobraProtocolError {}

/// Parses bounded standard output from Cobra's hidden `__complete` command.
///
/// The final line must contain `:<directive>`. Earlier lines contain either a
/// candidate or `candidate<TAB>description`. A final LF is optional and CRLF
/// framing is accepted; control characters inside fields are rejected.
///
/// # Errors
///
/// Returns an error for excessive, malformed, failed, unsafe, or unsupported
/// output. Callers must discard the complete response on any error.
pub fn parse_cobra_output(output: &[u8]) -> Result<CobraCompletion, CobraProtocolError> {
    if output.len() > MAX_COBRA_OUTPUT_BYTES {
        return Err(CobraProtocolError::OutputTooLarge {
            size: output.len(),
            limit: MAX_COBRA_OUTPUT_BYTES,
        });
    }
    let text = std::str::from_utf8(output).map_err(|_| CobraProtocolError::InvalidUtf8)?;
    if text.is_empty() {
        return Err(CobraProtocolError::MissingDirective);
    }

    let has_final_lf = text.ends_with('\n');
    let body = text.strip_suffix('\n').unwrap_or(text);
    let mut lines = body.split('\n').collect::<Vec<_>>();
    let directive_line = lines.pop().ok_or(CobraProtocolError::MissingDirective)?;
    let directive_line = if has_final_lf {
        strip_cr(directive_line)
    } else {
        directive_line
    };
    let directive = parse_directive(directive_line)?;
    let mut candidates = parse_candidates(&lines)?;

    if !directive.keep_order() {
        candidates.sort_by(|left, right| {
            left.value
                .cmp(&right.value)
                .then_with(|| left.description.cmp(&right.description))
        });
    }

    let file_completion = file_completion(directive, &candidates)?;
    if matches!(
        file_completion,
        CobraFileCompletion::FilterExtensions(_) | CobraFileCompletion::FilterDirectories { .. }
    ) {
        candidates.clear();
    }

    Ok(CobraCompletion {
        candidates,
        directive,
        file_completion,
    })
}

fn parse_directive(line: &str) -> Result<CobraDirective, CobraProtocolError> {
    let Some(digits) = line.strip_prefix(':') else {
        return Err(CobraProtocolError::MissingDirective);
    };
    if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(CobraProtocolError::MalformedDirective);
    }
    let bits = digits
        .parse::<u32>()
        .map_err(|_| CobraProtocolError::MalformedDirective)?;
    if bits & !KNOWN_DIRECTIVE_BITS != 0 {
        return Err(CobraProtocolError::UnknownDirectiveBits(
            bits & !KNOWN_DIRECTIVE_BITS,
        ));
    }
    if bits & DIRECTIVE_ERROR != 0 {
        return Err(CobraProtocolError::ErrorDirective);
    }
    if bits & DIRECTIVE_FILTER_FILE_EXTENSIONS != 0 && bits & DIRECTIVE_FILTER_DIRECTORIES != 0 {
        return Err(CobraProtocolError::ConflictingFileDirectives);
    }
    Ok(CobraDirective(bits))
}

fn parse_candidates(lines: &[&str]) -> Result<Vec<CobraCandidate>, CobraProtocolError> {
    if lines.len() > MAX_COBRA_CANDIDATES {
        return Err(CobraProtocolError::TooManyCandidates {
            limit: MAX_COBRA_CANDIDATES,
        });
    }

    lines
        .iter()
        .copied()
        .enumerate()
        .map(|(index, line)| parse_candidate(strip_cr(line), index + 1))
        .collect()
}

fn parse_candidate(line: &str, line_number: usize) -> Result<CobraCandidate, CobraProtocolError> {
    if line.is_empty() {
        return Err(CobraProtocolError::MalformedCandidate { line: line_number });
    }
    let (value, description) = line.split_once('\t').unwrap_or((line, ""));
    if value.is_empty() {
        return Err(CobraProtocolError::MalformedCandidate { line: line_number });
    }
    if value.chars().any(char::is_control) || description.chars().any(char::is_control) {
        return Err(CobraProtocolError::ControlText { line: line_number });
    }
    Ok(CobraCandidate {
        value: value.to_owned(),
        description: description.to_owned(),
    })
}

fn file_completion(
    directive: CobraDirective,
    candidates: &[CobraCandidate],
) -> Result<CobraFileCompletion, CobraProtocolError> {
    if directive.contains(DIRECTIVE_FILTER_FILE_EXTENSIONS) {
        return Ok(CobraFileCompletion::FilterExtensions(
            candidates
                .iter()
                .map(|candidate| candidate.value.clone())
                .collect(),
        ));
    }
    if directive.contains(DIRECTIVE_FILTER_DIRECTORIES) {
        if candidates.len() > 1 {
            return Err(CobraProtocolError::TooManyDirectoryFilters);
        }
        return Ok(CobraFileCompletion::FilterDirectories {
            within: candidates.first().map(|candidate| candidate.value.clone()),
        });
    }
    if directive.no_file_completion() {
        return Ok(CobraFileCompletion::Disabled);
    }
    Ok(CobraFileCompletion::Default)
}

fn strip_cr(line: &str) -> &str {
    line.strip_suffix('\r').unwrap_or(line)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn completion_budget_is_hard_coded_to_three_hundred_milliseconds() {
        assert_eq!(COBRA_COMPLETION_TIMEOUT, Duration::from_millis(300));
    }

    #[test]
    fn executable_validation_accepts_only_safe_basenames() {
        assert_eq!(CobraExecutable::new("kubectl").unwrap().as_str(), "kubectl");
        assert_eq!(CobraExecutable::new("gh.exe").unwrap().as_str(), "gh.exe");

        for invalid in [
            "",
            ".",
            "..",
            "/usr/bin/kubectl",
            "bin/kubectl",
            "bin\\kubectl",
            "gh\n",
        ] {
            assert_eq!(
                CobraExecutable::new(invalid),
                Err(CobraProtocolError::InvalidExecutableBasename)
            );
        }
    }

    #[test]
    fn request_keeps_every_protocol_argument_structured() {
        let request = CobraRequest::new(
            "kubectl",
            ["get", "pods", "--selector=app=study group"],
            "tr",
        )
        .unwrap();

        assert_eq!(request.executable().as_str(), "kubectl");
        assert_eq!(
            request.committed_arguments(),
            ["get", "pods", "--selector=app=study group"]
        );
        assert_eq!(request.partial(), "tr");
        assert_eq!(
            request.argv(),
            [
                "__complete",
                "get",
                "pods",
                "--selector=app=study group",
                "tr"
            ]
        );

        let trailing_space = CobraRequest::new("kubectl", ["get"], "").unwrap();
        assert_eq!(trailing_space.argv(), ["__complete", "get", ""]);
        assert_eq!(
            CobraRequest::new("kubectl", ["bad\0argument"], ""),
            Err(CobraProtocolError::InvalidArgument)
        );
    }

    #[test]
    fn cache_key_includes_binary_path_mtime_cwd_arguments_and_partial() {
        let request = CobraRequest::new("kubectl", ["get", "pods"], "tr").unwrap();
        let modified = SystemTime::UNIX_EPOCH + Duration::from_secs(42);
        let key = request.success_cache_key(
            CobraBinaryIdentity::new("/opt/bin/kubectl", modified),
            "/srv/greendale",
        );

        assert_eq!(key.binary.resolved_path, PathBuf::from("/opt/bin/kubectl"));
        assert_eq!(key.binary.modified, modified);
        assert_eq!(key.working_directory, PathBuf::from("/srv/greendale"));
        assert_eq!(key.committed_arguments, ["get", "pods"]);
        assert_eq!(key.partial, "tr");

        let other_partial = CobraRequest::new("kubectl", ["get", "pods"], "tro")
            .unwrap()
            .success_cache_key(
                CobraBinaryIdentity::new("/opt/bin/kubectl", modified),
                "/srv/greendale",
            );
        let other_arguments = CobraRequest::new("kubectl", ["get pods"], "tr")
            .unwrap()
            .success_cache_key(
                CobraBinaryIdentity::new("/opt/bin/kubectl", modified),
                "/srv/greendale",
            );
        let other_mtime = request.success_cache_key(
            CobraBinaryIdentity::new("/opt/bin/kubectl", modified + Duration::from_secs(1)),
            "/srv/greendale",
        );
        let other_path = request.success_cache_key(
            CobraBinaryIdentity::new("/usr/bin/kubectl", modified),
            "/srv/greendale",
        );
        let other_cwd = request.success_cache_key(
            CobraBinaryIdentity::new("/opt/bin/kubectl", modified),
            "/srv/city-college",
        );

        assert_ne!(key, other_partial);
        assert_ne!(key, other_arguments);
        assert_ne!(key, other_mtime);
        assert_ne!(key, other_path);
        assert_ne!(key, other_cwd);
    }

    #[test]
    fn parses_descriptions_and_sorts_without_keep_order() {
        let completion =
            parse_cobra_output(b"troy\tair-conditioning repair\nabed\tfilm studies\n:4\n").unwrap();

        assert_eq!(
            completion.candidates,
            vec![
                CobraCandidate {
                    value: "abed".into(),
                    description: "film studies".into(),
                },
                CobraCandidate {
                    value: "troy".into(),
                    description: "air-conditioning repair".into(),
                },
            ]
        );
        assert_eq!(completion.directive.bits(), DIRECTIVE_NO_FILE_COMPLETION);
        assert_eq!(completion.file_completion, CobraFileCompletion::Disabled);
    }

    #[test]
    fn preserves_order_and_no_space_metadata() {
        let bits = DIRECTIVE_KEEP_ORDER | DIRECTIVE_NO_SPACE;
        let output = format!("troy\nabed\n:{bits}\n");
        let completion = parse_cobra_output(output.as_bytes()).unwrap();

        assert_eq!(
            completion
                .candidates
                .iter()
                .map(|candidate| candidate.value.as_str())
                .collect::<Vec<_>>(),
            ["troy", "abed"]
        );
        assert!(completion.directive.keep_order());
        assert!(completion.directive.no_space());
    }

    #[test]
    fn turns_extension_and_directory_directives_into_filesystem_metadata() {
        let extensions = parse_cobra_output(b"yaml\njson\n:8\n").unwrap();
        assert!(extensions.candidates.is_empty());
        assert_eq!(
            extensions.file_completion,
            CobraFileCompletion::FilterExtensions(vec!["json".into(), "yaml".into()])
        );

        let directories = parse_cobra_output(b"campus/build\n:16\n").unwrap();
        assert!(directories.candidates.is_empty());
        assert_eq!(
            directories.file_completion,
            CobraFileCompletion::FilterDirectories {
                within: Some("campus/build".into())
            }
        );

        let current_directory = parse_cobra_output(b":16\n").unwrap();
        assert_eq!(
            current_directory.file_completion,
            CobraFileCompletion::FilterDirectories { within: None }
        );
    }

    #[test]
    fn accepts_no_candidates_no_description_and_crlf_framing() {
        let empty = parse_cobra_output(b":0").unwrap();
        assert!(empty.candidates.is_empty());

        let candidate = parse_cobra_output(b"greendale\r\n:0\r\n").unwrap();
        assert_eq!(
            candidate.candidates,
            vec![CobraCandidate {
                value: "greendale".into(),
                description: String::new(),
            }]
        );
    }

    #[test]
    fn rejects_failed_unknown_missing_and_malformed_directives() {
        for (output, error) in [
            (&b"candidate\n:1\n"[..], CobraProtocolError::ErrorDirective),
            (
                &b"candidate\n:64\n"[..],
                CobraProtocolError::UnknownDirectiveBits(64),
            ),
            (&b"candidate\n"[..], CobraProtocolError::MissingDirective),
            (
                &b"candidate\n:+2\n"[..],
                CobraProtocolError::MalformedDirective,
            ),
            (
                &b"candidate\n:-1\n"[..],
                CobraProtocolError::MalformedDirective,
            ),
            (
                &b"candidate\n: 2\n"[..],
                CobraProtocolError::MalformedDirective,
            ),
            (
                &b"candidate\n:24\n"[..],
                CobraProtocolError::ConflictingFileDirectives,
            ),
        ] {
            assert_eq!(parse_cobra_output(output), Err(error));
        }
    }

    #[test]
    fn rejects_malformed_candidates_and_control_text() {
        for (output, error) in [
            (
                &b"\n:0\n"[..],
                CobraProtocolError::MalformedCandidate { line: 1 },
            ),
            (
                &b"\tdescription\n:0\n"[..],
                CobraProtocolError::MalformedCandidate { line: 1 },
            ),
            (
                &b"candidate\tdescription\tescape\n:0\n"[..],
                CobraProtocolError::ControlText { line: 1 },
            ),
            (
                &b"candidate\tescape\x1b[31m\n:0\n"[..],
                CobraProtocolError::ControlText { line: 1 },
            ),
            (
                &b"candidate\rinside\n:0\n"[..],
                CobraProtocolError::ControlText { line: 1 },
            ),
            (
                &b"candidate\n:0\r"[..],
                CobraProtocolError::MalformedDirective,
            ),
        ] {
            assert_eq!(parse_cobra_output(output), Err(error));
        }
    }

    #[test]
    fn rejects_invalid_utf8_and_hard_limit_violations() {
        assert_eq!(
            parse_cobra_output(&[0xff, b'\n', b':', b'0']),
            Err(CobraProtocolError::InvalidUtf8)
        );
        let oversized = vec![b'x'; MAX_COBRA_OUTPUT_BYTES + 1];
        assert_eq!(
            parse_cobra_output(&oversized),
            Err(CobraProtocolError::OutputTooLarge {
                size: MAX_COBRA_OUTPUT_BYTES + 1,
                limit: MAX_COBRA_OUTPUT_BYTES,
            })
        );

        let mut candidates = "candidate\n".repeat(MAX_COBRA_CANDIDATES + 1);
        candidates.push_str(":0\n");
        assert_eq!(
            parse_cobra_output(candidates.as_bytes()),
            Err(CobraProtocolError::TooManyCandidates {
                limit: MAX_COBRA_CANDIDATES,
            })
        );
    }

    #[test]
    fn rejects_ambiguous_directory_filter_payloads() {
        assert_eq!(
            parse_cobra_output(b"first\nsecond\n:16\n"),
            Err(CobraProtocolError::TooManyDirectoryFilters)
        );
    }
}
