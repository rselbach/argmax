//! Bounded execution of curated dynamic generators and Cobra inference.
//!
//! This boundary turns declarative generator metadata into exact, shell-free
//! local process requests or bounded file reads. Captured values remain inert:
//! they become [`Suggestion`] edits and are never written to a terminal.

use std::collections::BTreeSet;
use std::ffi::{OsStr, OsString};
use std::fs::{self, File, Metadata};
use std::io::{self, Read};
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use serde_json::Value as JsonValue;

use crate::completion::{
    CancellationToken, CompletionQuery, GeneratorKind, GeneratorSpec, InsertionBehavior, SpecIndex,
    SpecResolution, Suggestion, SuggestionSource, TextEdit, TokenizedLine, tokenize,
};
use crate::process_runner::{LocalProcessRequest, run_local_process};

use super::{
    COBRA_COMPLETION_TIMEOUT, CobraBinaryIdentity, CobraCacheKey, CobraCandidate, CobraCompletion,
    CobraFileCompletion, CobraRequest, DynamicCacheKey, DynamicItem, DynamicItemKind,
    DynamicMetadata, DynamicResourceKind, DynamicResultCache, FilesystemOptions, GitBranchOptions,
    GitBranchScope, MAX_COBRA_OUTPUT_BYTES, MAX_DYNAMIC_ITEMS, MAX_DYNAMIC_OUTPUT_BYTES,
    PathExecutable, PathExecutableCache, ShellKind, environment_variable_items,
    filesystem_suggestions, parse_cobra_output, parse_git_branches, parse_git_commits,
    parse_git_remotes, parse_git_stashes, parse_git_tags, parse_just_recipes, parse_make_targets,
    parse_processes, parse_resource_lines, parse_ssh_hosts, parse_zoxide_directories, quote_path,
};

/// Maximum bytes read from one project or user configuration file.
pub const MAX_GENERATOR_FILE_BYTES: usize = MAX_DYNAMIC_OUTPUT_BYTES;
/// Maximum successful Cobra responses retained for one interactive session.
pub const MAX_COBRA_CACHE_ENTRIES: usize = 128;
/// Lifetime of a successfully parsed Cobra response.
pub const COBRA_CACHE_TTL: Duration = Duration::from_secs(5);

const MAX_DIRECT_FILES: usize = 8_192;
const MAX_MANIFEST_VALUES: usize = MAX_DYNAMIC_ITEMS;
const MAX_IDENTITY_BYTES: usize = 8 * 1024;

/// Command-line configuration applied to every git generator invocation.
///
/// A working directory carries its own `.git/config`, which the repository
/// author controls rather than the user. Both `core.fsmonitor` and
/// `core.hooksPath` name programs git executes during otherwise read-only
/// queries, so a generator running in an untrusted checkout would execute
/// repository-supplied commands. Command-line `-c` wins over every file-based
/// source, so neutralizing them here cannot be overridden by the repository.
///
/// `core.fsmonitor` is cleared rather than set to `false`: git releases before
/// 2.36 read the value as a hook path, where `false` resolves to an executable
/// on `PATH`, while an empty value is inert for every version.
const GIT_READ_ONLY_CONFIG: [&str; 6] = [
    "-c",
    "color.ui=false",
    "-c",
    "core.fsmonitor=",
    "-c",
    "core.hooksPath=/dev/null",
];

/// Prefixes [`GIT_READ_ONLY_CONFIG`] to one git generator's own arguments.
fn git_arguments<'a>(rest: &[&'a str]) -> Vec<&'a str> {
    GIT_READ_ONLY_CONFIG
        .iter()
        .copied()
        .chain(rest.iter().copied())
        .collect()
}

/// Git-specific dynamic completion behavior.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct GitGeneratorSettings {
    /// Hide the active local branch where selecting it is a no-op.
    pub filter_active_branch: bool,
    /// Prefer a local branch over equivalent remote branch suffixes.
    pub deduplicate_branches: bool,
}

impl Default for GitGeneratorSettings {
    fn default() -> Self {
        Self {
            filter_active_branch: true,
            deduplicate_branches: true,
        }
    }
}

/// Immutable ambient inputs used by local generators.
#[derive(Clone, Copy)]
pub struct GeneratorExecutionContext<'a> {
    /// Active shell, used only to quote inert insertion text.
    pub shell: ShellKind,
    /// Authoritative `PATH` snapshot for executable resolution.
    pub path: &'a OsStr,
    /// Home directory used for home-relative files and SSH configuration.
    pub home_directory: Option<&'a Path>,
    /// Structured environment-variable names; values are neither accepted nor retained.
    pub environment_names: &'a [String],
    /// Whether filesystem completion includes dot-prefixed entries.
    pub include_hidden_files: bool,
    /// Git branch filtering settings.
    pub git: GitGeneratorSettings,
}

impl std::fmt::Debug for GeneratorExecutionContext<'_> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("GeneratorExecutionContext")
            .field("shell", &self.shell)
            .field("path_bytes", &self.path.as_encoded_bytes().len())
            .field(
                "home_bytes",
                &self
                    .home_directory
                    .map_or(0, |path| path.as_os_str().as_encoded_bytes().len()),
            )
            .field("environment_name_count", &self.environment_names.len())
            .field("include_hidden_files", &self.include_hidden_files)
            .field("git", &self.git)
            .finish()
    }
}

impl<'a> GeneratorExecutionContext<'a> {
    /// Creates a context with conservative defaults and no retained environment names.
    #[must_use]
    pub const fn new(shell: ShellKind, path: &'a OsStr) -> Self {
        Self {
            shell,
            path,
            home_directory: None,
            environment_names: &[],
            include_hidden_files: false,
            git: GitGeneratorSettings {
                filter_active_branch: true,
                deduplicate_branches: true,
            },
        }
    }
}

/// Why Cobra inference is or is not allowed for an executable name.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CobraInferenceEligibility {
    /// No curated root claims this safe executable basename.
    Eligible,
    /// A curated root or alias exists and takes precedence.
    CuratedDefinition,
    /// The candidate is not a safe executable basename.
    InvalidExecutable,
}

/// Determines whether Cobra may infer a command not covered by the curated index.
#[must_use]
pub fn cobra_inference_eligibility(
    index: &SpecIndex,
    executable: &str,
) -> CobraInferenceEligibility {
    if CobraRequest::new(executable, std::iter::empty::<String>(), "").is_err() {
        return CobraInferenceEligibility::InvalidExecutable;
    }
    let curated = index.roots().iter().any(|root| {
        root.name.eq_ignore_ascii_case(executable)
            || root
                .aliases
                .iter()
                .any(|alias| alias.eq_ignore_ascii_case(executable))
    });
    if curated {
        CobraInferenceEligibility::CuratedDefinition
    } else {
        CobraInferenceEligibility::Eligible
    }
}

/// Successful Cobra inference plus its explicit filesystem behavior.
#[derive(Clone, Debug, PartialEq)]
pub struct CobraExecution {
    /// Inert candidates and any requested filesystem fallback.
    pub suggestions: Vec<Suggestion>,
    /// Validated Cobra filesystem directive.
    pub file_completion: CobraFileCompletion,
    /// Whether the parsed protocol response came from the session cache.
    pub cache_hit: bool,
}

/// Session-local dynamic and Cobra executor.
///
/// Only successful, fully parsed responses are admitted to either cache.
#[derive(Default)]
pub struct DynamicExecutor {
    dynamic_results: DynamicResultCache,
    cobra_results: Vec<CobraCacheEntry>,
    paths: PathExecutableCache,
}

impl std::fmt::Debug for DynamicExecutor {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("DynamicExecutor")
            .field("dynamic_results", &self.dynamic_results)
            .field("cobra_result_count", &self.cobra_results.len())
            .finish_non_exhaustive()
    }
}

impl DynamicExecutor {
    /// Creates empty session-local caches.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Executes every generator active at the cursor independently.
    ///
    /// Invalid queries, missing tools or files, nonzero exits, malformed output,
    /// cancellation, and timeouts contribute no dynamic rows. Other generators
    /// remain eligible, and static provider output is unaffected.
    #[must_use]
    pub fn complete_curated(
        &mut self,
        index: &SpecIndex,
        query: &CompletionQuery,
        context: GeneratorExecutionContext<'_>,
        cancellation: &CancellationToken,
    ) -> Vec<Suggestion> {
        let mut runner = NativeCommandRunner;
        self.complete_curated_with(index, query, context, cancellation, &mut runner)
    }

    /// Runs Cobra inference for an installed, uncurated executable basename.
    ///
    /// The hidden protocol is invoked with exact structured arguments and a hard
    /// 300 ms budget. Transient resolution, execution, and parse failures return
    /// `None` and are never admitted to the cache.
    #[must_use]
    pub fn complete_cobra(
        &mut self,
        index: &SpecIndex,
        query: &CompletionQuery,
        context: GeneratorExecutionContext<'_>,
        cancellation: &CancellationToken,
    ) -> Option<CobraExecution> {
        let mut runner = NativeCommandRunner;
        self.complete_cobra_with(index, query, context, cancellation, &mut runner)
    }

    /// Invalidates local result caches after an explicit workspace change.
    pub fn invalidate_cwd(&mut self, cwd: &Path) {
        self.dynamic_results.invalidate_cwd(cwd);
        self.cobra_results.clear();
        self.paths.invalidate();
    }

    fn complete_curated_with<R: CommandRunner>(
        &mut self,
        index: &SpecIndex,
        query: &CompletionQuery,
        context: GeneratorExecutionContext<'_>,
        cancellation: &CancellationToken,
        runner: &mut R,
    ) -> Vec<Suggestion> {
        if cancellation.is_cancelled() || !query.cwd.is_absolute() {
            return Vec::new();
        }
        let Ok(line) = tokenize(&query.line, query.cursor) else {
            return Vec::new();
        };
        let Some(resolution) = index.resolve(&line) else {
            return Vec::new();
        };
        let arguments = line
            .committed_tokens()
            .iter()
            .map(|token| token.cooked.clone())
            .collect::<Vec<_>>();
        let mut suggestions = Vec::new();

        for spec in resolution.active_generators() {
            if cancellation.is_cancelled() {
                break;
            }
            let generated = self.execute_generator(
                spec,
                query,
                &line,
                &resolution,
                &arguments,
                context,
                cancellation,
                runner,
            );
            suggestions.extend(generated);
        }
        let mut seen = BTreeSet::new();
        suggestions.retain(|suggestion| seen.insert(suggestion.identity().to_owned()));
        suggestions
    }

    #[allow(clippy::too_many_arguments)]
    fn execute_generator<R: CommandRunner>(
        &mut self,
        spec: &GeneratorSpec,
        query: &CompletionQuery,
        line: &TokenizedLine,
        resolution: &SpecResolution<'_>,
        arguments: &[String],
        context: GeneratorExecutionContext<'_>,
        cancellation: &CancellationToken,
        runner: &mut R,
    ) -> Vec<Suggestion> {
        if let GeneratorKind::Filesystem(filesystem) = &spec.kind {
            let Some(deadline) = Instant::now().checked_add(spec.timeout) else {
                return Vec::new();
            };
            let options = FilesystemOptions {
                include_hidden: context.include_hidden_files,
                directory_only: filesystem.directory_only,
                extensions: filesystem.extensions.clone(),
                home_directory: context.home_directory.map(Path::to_path_buf),
                file_insertion: InsertionBehavior::AppendSpace,
            };
            return super::filesystem::filesystem_suggestions_bounded(
                query,
                context.shell,
                &options,
                filesystem.max_entries,
                || cancellation.is_cancelled() || Instant::now() >= deadline,
            )
            .into_iter()
            .take(spec.max_results)
            .collect();
        }

        let now = Instant::now();
        let prepared = self.prepare_generator(spec, query, resolution, context);
        let Some(prepared) = prepared else {
            return Vec::new();
        };
        let mut cache_arguments = arguments.to_vec();
        if matches!(&spec.kind, GeneratorKind::GitBranches) {
            cache_arguments.push(format!(
                "git-filter-active-branch:{}",
                context.git.filter_active_branch
            ));
            cache_arguments.push(format!(
                "git-deduplicate-branches:{}",
                context.git.deduplicate_branches
            ));
        }
        let cache_key = prepared.cache_identity.as_ref().and_then(|identity| {
            cache_arguments.extend(identity.iter().cloned());
            DynamicCacheKey::new(spec, &query.cwd, cache_arguments, &resolution.partial).ok()
        });
        if let Some(items) = cache_key
            .as_ref()
            .and_then(|key| self.dynamic_results.get(key, now))
        {
            return dynamic_items_to_suggestions(
                items,
                query,
                line,
                resolution,
                context.shell,
                spec.max_results,
            );
        }

        let Ok(mut items) = prepared.execute(spec, &query.cwd, context, cancellation, runner)
        else {
            return Vec::new();
        };
        if cancellation.is_cancelled() {
            return Vec::new();
        }
        items.retain(|item| dynamic_item_matches(item, &resolution.partial));
        items.truncate(spec.max_results);
        let suggestions = dynamic_items_to_suggestions(
            &items,
            query,
            line,
            resolution,
            context.shell,
            spec.max_results,
        );
        if let Some(key) = cache_key {
            let _admission = self.dynamic_results.insert_success(key, items, now);
        }
        suggestions
    }

    #[allow(clippy::too_many_lines)]
    fn prepare_generator(
        &mut self,
        spec: &GeneratorSpec,
        query: &CompletionQuery,
        resolution: &SpecResolution<'_>,
        context: GeneratorExecutionContext<'_>,
    ) -> Option<PreparedGenerator> {
        let mut process = |program: &'static str, arguments: &[&str], parser| {
            self.resolve_program(program, context.path, &query.cwd)
                .map(|binary| PreparedGenerator {
                    cache_identity: Some(binary.cache_identity()),
                    source: GeneratorSource::Process {
                        binary,
                        arguments: arguments.iter().map(OsString::from).collect(),
                        parser,
                    },
                })
        };

        match spec.kind {
            GeneratorKind::GitBranches => process(
                "git",
                &git_arguments(&[
                    "for-each-ref",
                    "--format=%(refname)%09%(HEAD)",
                    "refs/heads",
                    "refs/remotes",
                ]),
                ProcessParser::GitBranches(context.git),
            ),
            GeneratorKind::GitRemotes => process(
                "git",
                &git_arguments(&["remote"]),
                ProcessParser::GitRemotes,
            ),
            GeneratorKind::GitTags => process(
                "git",
                &git_arguments(&["tag", "--list"]),
                ProcessParser::GitTags,
            ),
            GeneratorKind::GitStashes => process(
                "git",
                &git_arguments(&["stash", "list", "--format=%gd%x09%gs"]),
                ProcessParser::GitStashes,
            ),
            GeneratorKind::GitCommits => {
                let maximum = format!("--max-count={}", spec.max_results);
                let binary = self.resolve_program("git", context.path, &query.cwd)?;
                Some(PreparedGenerator {
                    cache_identity: Some(binary.cache_identity()),
                    source: GeneratorSource::Process {
                        binary,
                        arguments: git_arguments(&["log", &maximum, "--format=%h%x09%s"])
                            .into_iter()
                            .map(OsString::from)
                            .collect(),
                        parser: ProcessParser::GitCommits,
                    },
                })
            }
            GeneratorKind::GitFiles => process(
                "git",
                &git_arguments(&[
                    "ls-files",
                    "-z",
                    "--cached",
                    "--modified",
                    "--others",
                    "--exclude-standard",
                ]),
                ProcessParser::GitFiles,
            ),
            GeneratorKind::PackageScripts => Self::nearest_snapshot(&query.cwd, &["package.json"])
                .map(|snapshot| PreparedGenerator::file(snapshot, FileParser::PackageScripts)),
            GeneratorKind::MakeTargets => {
                Self::nearest_snapshot(&query.cwd, &["GNUmakefile", "makefile", "Makefile"])
                    .map(|snapshot| PreparedGenerator::file(snapshot, FileParser::MakeTargets))
            }
            GeneratorKind::JustRecipes => {
                Self::nearest_snapshot(&query.cwd, &["justfile", "Justfile"])
                    .map(|snapshot| PreparedGenerator::file(snapshot, FileParser::JustRecipes))
            }
            GeneratorKind::DockerContainers => process(
                "docker",
                &["ps", "--format", "{{.Names}}\tDocker container {{.ID}}"],
                ProcessParser::Resource(DynamicResourceKind::DockerContainer),
            ),
            GeneratorKind::DockerImages => process(
                "docker",
                &[
                    "image",
                    "ls",
                    "--format",
                    "{{.Repository}}:{{.Tag}}\tDocker image {{.ID}}",
                ],
                ProcessParser::Resource(DynamicResourceKind::DockerImage),
            ),
            GeneratorKind::SshHosts => Self::prepare_ssh(context.home_directory),
            GeneratorKind::ZoxideDirectories => process(
                "zoxide",
                &["query", "--list", "--score"],
                ProcessParser::Zoxide,
            ),
            GeneratorKind::Packages => self.prepare_packages(query, resolution, context.path),
            GeneratorKind::Processes => {
                process("ps", &["-axo", "pid=,comm="], ProcessParser::Processes)
            }
            GeneratorKind::Services => process(
                "systemctl",
                &["list-unit-files", "--no-legend", "--no-pager", "--plain"],
                ProcessParser::Services,
            ),
            GeneratorKind::EnvironmentVariables => Some(PreparedGenerator {
                cache_identity: None,
                source: GeneratorSource::Environment(context.environment_names.to_vec()),
            }),
            GeneratorKind::FileTypes => Some(PreparedGenerator {
                cache_identity: directory_identity(&query.cwd).map(|identity| vec![identity]),
                source: GeneratorSource::FileTypes(query.cwd.clone()),
            }),
            GeneratorKind::Filesystem(_) => None,
        }
    }

    fn prepare_packages(
        &mut self,
        query: &CompletionQuery,
        resolution: &SpecResolution<'_>,
        path: &OsStr,
    ) -> Option<PreparedGenerator> {
        let root = resolution.path.first()?.as_str();
        let (names, parser) = match root {
            "npm" | "pnpm" | "yarn" | "bun" => (&["package.json"][..], FileParser::NodePackages),
            "cargo" => (&["Cargo.toml"][..], FileParser::CargoPackages),
            "pip" | "pip3" | "python" | "python3" | "poetry" => (
                &["pyproject.toml", "requirements.txt"][..],
                FileParser::PythonPackages,
            ),
            "go" => (&["go.mod"][..], FileParser::GoPackages),
            "apt" | "apt-get" | "dpkg" => {
                let binary = self.resolve_program("dpkg-query", path, &query.cwd)?;
                return Some(PreparedGenerator {
                    cache_identity: Some(binary.cache_identity()),
                    source: GeneratorSource::Process {
                        binary,
                        arguments: ["-W".into(), "-f=${binary:Package}\\n".into()].into(),
                        parser: ProcessParser::Resource(DynamicResourceKind::Package),
                    },
                });
            }
            "brew" => {
                let binary = self.resolve_program("brew", path, &query.cwd)?;
                return Some(PreparedGenerator {
                    cache_identity: Some(binary.cache_identity()),
                    source: GeneratorSource::Process {
                        binary,
                        arguments: ["list".into(), "--formula".into()].into(),
                        parser: ProcessParser::Resource(DynamicResourceKind::Package),
                    },
                });
            }
            "pacman" | "yay" | "paru" => {
                let binary = self.resolve_program(root, path, &query.cwd)?;
                return Some(PreparedGenerator {
                    cache_identity: Some(binary.cache_identity()),
                    source: GeneratorSource::Process {
                        binary,
                        arguments: ["-Qq".into()].into(),
                        parser: ProcessParser::Resource(DynamicResourceKind::Package),
                    },
                });
            }
            "dnf" | "yum" => {
                let binary = self.resolve_program("rpm", path, &query.cwd)?;
                return Some(PreparedGenerator {
                    cache_identity: Some(binary.cache_identity()),
                    source: GeneratorSource::Process {
                        binary,
                        arguments: ["-qa".into(), "--qf".into(), "%{NAME}\n".into()].into(),
                        parser: ProcessParser::Resource(DynamicResourceKind::Package),
                    },
                });
            }
            _ => return None,
        };
        Self::nearest_snapshot(&query.cwd, names)
            .map(|snapshot| PreparedGenerator::file(snapshot, parser))
    }

    fn prepare_ssh(home: Option<&Path>) -> Option<PreparedGenerator> {
        let ssh = home?.join(".ssh");
        let known_hosts = read_optional_snapshot(&ssh.join("known_hosts")).ok()?;
        let config = read_optional_snapshot(&ssh.join("config")).ok()?;
        if known_hosts.is_none() && config.is_none() {
            return None;
        }
        let mut cache_identity = Vec::new();
        if let Some(snapshot) = &known_hosts {
            cache_identity.extend(snapshot.cache_identity());
        }
        if let Some(snapshot) = &config {
            cache_identity.extend(snapshot.cache_identity());
        }
        Some(PreparedGenerator {
            cache_identity: Some(cache_identity),
            source: GeneratorSource::Ssh {
                known_hosts,
                config,
            },
        })
    }

    fn nearest_snapshot(cwd: &Path, names: &[&str]) -> Option<FileSnapshot> {
        for directory in cwd.ancestors() {
            for name in names {
                match read_optional_snapshot(&directory.join(name)) {
                    Ok(Some(snapshot)) => return Some(snapshot),
                    Ok(None) => {}
                    Err(()) => return None,
                }
            }
        }
        None
    }

    fn resolve_program(&mut self, name: &str, path: &OsStr, cwd: &Path) -> Option<ResolvedBinary> {
        self.paths
            .executables(path, cwd)
            .iter()
            .find(|candidate| candidate.name == name)
            .and_then(ResolvedBinary::from_executable)
    }

    fn complete_cobra_with<R: CommandRunner>(
        &mut self,
        index: &SpecIndex,
        query: &CompletionQuery,
        context: GeneratorExecutionContext<'_>,
        cancellation: &CancellationToken,
        runner: &mut R,
    ) -> Option<CobraExecution> {
        if cancellation.is_cancelled() || !query.cwd.is_absolute() {
            return None;
        }
        let line = tokenize(&query.line, query.cursor).ok()?;
        let committed = line.committed_tokens();
        let executable = committed.first()?.cooked.as_str();
        match cobra_inference_eligibility(index, executable) {
            CobraInferenceEligibility::InvalidExecutable => return None,
            CobraInferenceEligibility::CuratedDefinition
                if curated_resolution_is_usable(index, &line) =>
            {
                return None;
            }
            CobraInferenceEligibility::Eligible | CobraInferenceEligibility::CuratedDefinition => {}
        }
        let request = CobraRequest::new(
            executable,
            committed[1..].iter().map(|token| token.cooked.clone()),
            line.active_token().cooked.clone(),
        )
        .ok()?;
        let binary =
            self.resolve_program(request.executable().as_str(), context.path, &query.cwd)?;
        let key = request.success_cache_key(
            CobraBinaryIdentity::new(binary.path.clone(), binary.modified),
            &query.cwd,
        );
        let now = Instant::now();
        let cached = self.cobra_cache_get(&key, now).cloned();
        let (completion, cache_hit) = if let Some(completion) = cached {
            (completion, true)
        } else {
            let plan = ProcessPlan {
                program: binary.path,
                arguments: request.argv().into_iter().map(OsString::from).collect(),
                cwd: query.cwd.clone(),
                timeout: COBRA_COMPLETION_TIMEOUT,
                output_limit: MAX_COBRA_OUTPUT_BYTES,
                path: context.path.to_os_string(),
                home: context.home_directory.map(Path::to_path_buf),
            };
            let output = runner.run(&plan).ok()?;
            if cancellation.is_cancelled() {
                return None;
            }
            let completion = parse_cobra_output(&output).ok()?;
            self.cobra_cache_insert(key, completion.clone(), now);
            (completion, false)
        };
        Some(cobra_completion_to_execution(
            &completion,
            query,
            &line,
            context,
            cache_hit,
        ))
    }

    fn cobra_cache_get(&mut self, key: &CobraCacheKey, now: Instant) -> Option<&CobraCompletion> {
        self.cobra_results.retain(|entry| now < entry.expires_at);
        let position = self
            .cobra_results
            .iter()
            .position(|entry| &entry.key == key)?;
        let entry = self.cobra_results.remove(position);
        self.cobra_results.push(entry);
        self.cobra_results.last().map(|entry| &entry.completion)
    }

    fn cobra_cache_insert(
        &mut self,
        key: CobraCacheKey,
        completion: CobraCompletion,
        now: Instant,
    ) {
        self.cobra_results.retain(|entry| now < entry.expires_at);
        if let Some(position) = self.cobra_results.iter().position(|entry| entry.key == key) {
            self.cobra_results.remove(position);
        }
        while self.cobra_results.len() >= MAX_COBRA_CACHE_ENTRIES {
            self.cobra_results.remove(0);
        }
        let Some(expires_at) = now.checked_add(COBRA_CACHE_TTL) else {
            return;
        };
        self.cobra_results.push(CobraCacheEntry {
            key,
            completion,
            expires_at,
        });
    }
}

fn curated_resolution_is_usable(index: &SpecIndex, line: &TokenizedLine) -> bool {
    index.resolve(line).is_some_and(|resolution| {
        !resolution.node.subcommands.is_empty()
            || !resolution.available_options().is_empty()
            || !resolution.node.generators.is_empty()
            || resolution.node.max_positionals.is_some()
    })
}

/// Converts normalized dynamic items into deterministic, inert spec suggestions.
#[must_use]
pub fn dynamic_items_to_suggestions(
    items: &[DynamicItem],
    query: &CompletionQuery,
    line: &TokenizedLine,
    resolution: &SpecResolution<'_>,
    shell: ShellKind,
    maximum: usize,
) -> Vec<Suggestion> {
    let range = dynamic_replacement_range(query, line, resolution);
    items
        .iter()
        .filter(|item| dynamic_item_matches(item, &resolution.partial))
        .take(maximum)
        .map(|item| {
            let icon = dynamic_icon(item.kind);
            let mut suggestion = Suggestion::new(
                TextEdit {
                    range: range.clone(),
                    replacement: quote_path(shell, &item.value),
                },
                &item.value,
                &item.description,
                icon,
                SuggestionSource::Spec,
                InsertionBehavior::AppendSpace,
                format!("dynamic:{icon}:{}", item.value),
            );
            suggestion.static_priority = 0.65;
            suggestion.confidence = 0.9;
            suggestion
        })
        .collect()
}

fn dynamic_item_matches(item: &DynamicItem, partial: &str) -> bool {
    let folded_partial = partial.to_lowercase();
    if item.value.to_lowercase().starts_with(&folded_partial) {
        return true;
    }
    let DynamicMetadata::GitBranch {
        scope: GitBranchScope::Remote { remote },
        ..
    } = &item.metadata
    else {
        return false;
    };
    item.value
        .strip_prefix(remote)
        .and_then(|suffix| suffix.strip_prefix('/'))
        .is_some_and(|suffix| suffix.to_lowercase().starts_with(&folded_partial))
}

fn dynamic_replacement_range(
    query: &CompletionQuery,
    line: &TokenizedLine,
    resolution: &SpecResolution<'_>,
) -> Range<usize> {
    let full = line.full_active_token();
    let end = full
        .raw
        .end
        .max(resolution.replacement.end)
        .min(query.line.len());
    resolution.replacement.start.min(end)..end
}

const fn dynamic_icon(kind: DynamicItemKind) -> &'static str {
    match kind {
        DynamicItemKind::GitBranch => "git-branch",
        DynamicItemKind::GitRemote => "git-remote",
        DynamicItemKind::GitTag => "git-tag",
        DynamicItemKind::GitStash => "git-stash",
        DynamicItemKind::GitCommit => "git-commit",
        DynamicItemKind::MakeTarget => "make-target",
        DynamicItemKind::JustRecipe => "just-recipe",
        DynamicItemKind::SshHost => "ssh-host",
        DynamicItemKind::ZoxideDirectory => "directory",
        DynamicItemKind::Process => "process",
        DynamicItemKind::EnvironmentVariable => "environment",
        DynamicItemKind::Resource(resource) => match resource {
            DynamicResourceKind::GitFile => "file",
            DynamicResourceKind::DockerContainer => "container",
            DynamicResourceKind::DockerImage => "image",
            DynamicResourceKind::Package => "package",
            DynamicResourceKind::PackageScript => "script",
            DynamicResourceKind::Service => "service",
            DynamicResourceKind::FileType => "file-type",
        },
    }
}

fn cobra_completion_to_execution(
    completion: &CobraCompletion,
    query: &CompletionQuery,
    line: &TokenizedLine,
    context: GeneratorExecutionContext<'_>,
    cache_hit: bool,
) -> CobraExecution {
    let range = line.active_token().raw.start..line.full_active_token().raw.end;
    let insertion = if completion.directive.no_space() {
        InsertionBehavior::Exact
    } else {
        InsertionBehavior::AppendSpace
    };
    let mut suggestions = cobra_candidates_to_suggestions(
        &completion.candidates,
        range.clone(),
        context.shell,
        insertion,
    );
    let file_completion = completion.file_completion.clone();
    let filesystem = match &file_completion {
        CobraFileCompletion::Default if suggestions.is_empty() => Some((false, Vec::new(), None)),
        CobraFileCompletion::FilterExtensions(extensions) => {
            Some((false, extensions.clone(), None))
        }
        CobraFileCompletion::FilterDirectories { within } => {
            Some((true, Vec::new(), within.as_deref()))
        }
        CobraFileCompletion::Default | CobraFileCompletion::Disabled => None,
    };
    if let Some((directory_only, extensions, within)) = filesystem {
        suggestions.extend(cobra_filesystem_suggestions(
            query,
            line,
            context,
            directory_only,
            extensions,
            within,
            range,
        ));
    }
    suggestions.truncate(MAX_DYNAMIC_ITEMS);
    CobraExecution {
        suggestions,
        file_completion,
        cache_hit,
    }
}

/// Converts validated Cobra candidates into inert inferred-spec suggestions.
#[must_use]
pub fn cobra_candidates_to_suggestions(
    candidates: &[CobraCandidate],
    replacement: Range<usize>,
    shell: ShellKind,
    insertion: InsertionBehavior,
) -> Vec<Suggestion> {
    candidates
        .iter()
        .take(MAX_DYNAMIC_ITEMS)
        .map(|candidate| {
            let mut suggestion = Suggestion::new(
                TextEdit {
                    range: replacement.clone(),
                    replacement: quote_path(shell, &candidate.value),
                },
                &candidate.value,
                &candidate.description,
                "inferred",
                SuggestionSource::SpecInferred,
                insertion,
                format!("cobra:{}", candidate.value),
            );
            suggestion.static_priority = 0.5;
            suggestion.confidence = 0.7;
            suggestion
        })
        .collect()
}

#[allow(clippy::too_many_arguments)]
fn cobra_filesystem_suggestions(
    query: &CompletionQuery,
    line: &TokenizedLine,
    context: GeneratorExecutionContext<'_>,
    directory_only: bool,
    extensions: Vec<String>,
    within: Option<&str>,
    replacement: Range<usize>,
) -> Vec<Suggestion> {
    let partial = &line.active_token().cooked;
    let logical = within.map_or_else(
        || partial.clone(),
        |directory| {
            let directory = directory.trim_end_matches('/');
            if directory.is_empty() {
                partial.clone()
            } else {
                format!("{directory}/{partial}")
            }
        },
    );
    let synthetic =
        CompletionQuery::new(logical.clone(), logical.len(), &query.cwd, query.generation);
    let Ok(synthetic) = synthetic else {
        return Vec::new();
    };
    let options = FilesystemOptions {
        include_hidden: context.include_hidden_files,
        directory_only,
        extensions,
        home_directory: context.home_directory.map(Path::to_path_buf),
        file_insertion: InsertionBehavior::AppendSpace,
    };
    filesystem_suggestions(&synthetic, context.shell, &options)
        .into_iter()
        .map(|mut suggestion| {
            suggestion.edit.range = replacement.clone();
            suggestion.source = SuggestionSource::SpecInferred;
            suggestion.sources = BTreeSet::from([SuggestionSource::SpecInferred]);
            suggestion
        })
        .collect()
}

struct CobraCacheEntry {
    key: CobraCacheKey,
    completion: CobraCompletion,
    expires_at: Instant,
}

#[derive(Clone, Debug)]
struct ResolvedBinary {
    path: PathBuf,
    modified: SystemTime,
}

impl ResolvedBinary {
    fn from_executable(executable: &PathExecutable) -> Option<Self> {
        let metadata = fs::metadata(&executable.path).ok()?;
        if !metadata.is_file() || !is_executable(&metadata) {
            return None;
        }
        Some(Self {
            path: executable.path.clone(),
            modified: metadata.modified().ok()?,
        })
    }

    fn cache_identity(&self) -> Vec<String> {
        vec![
            format!("binary-path:{}", encoded_path(&self.path)),
            format!("binary-mtime:{}", system_time_identity(self.modified)),
        ]
    }
}

struct PreparedGenerator {
    cache_identity: Option<Vec<String>>,
    source: GeneratorSource,
}

impl PreparedGenerator {
    fn file(snapshot: FileSnapshot, parser: FileParser) -> Self {
        Self {
            cache_identity: Some(snapshot.cache_identity()),
            source: GeneratorSource::File { snapshot, parser },
        }
    }

    fn execute<R: CommandRunner>(
        self,
        spec: &GeneratorSpec,
        cwd: &Path,
        context: GeneratorExecutionContext<'_>,
        cancellation: &CancellationToken,
        runner: &mut R,
    ) -> Result<Vec<DynamicItem>, ()> {
        if cancellation.is_cancelled() {
            return Err(());
        }
        match self.source {
            GeneratorSource::Process {
                binary,
                arguments,
                parser,
            } => {
                let started = Instant::now();
                let plan = ProcessPlan {
                    program: binary.path.clone(),
                    arguments,
                    cwd: cwd.to_path_buf(),
                    timeout: spec.timeout,
                    output_limit: MAX_DYNAMIC_OUTPUT_BYTES,
                    path: context.path.to_os_string(),
                    home: context.home_directory.map(Path::to_path_buf),
                };
                let output = runner.run(&plan)?;
                let ProcessParser::GitBranches(settings) = parser else {
                    return parser.parse(&output);
                };
                if cancellation.is_cancelled() {
                    return Err(());
                }
                let remaining = spec
                    .timeout
                    .checked_sub(started.elapsed())
                    .filter(|time| !time.is_zero())
                    .ok_or(())?;
                let remotes = runner.run(&ProcessPlan {
                    program: binary.path,
                    arguments: git_arguments(&["remote"])
                        .into_iter()
                        .map(OsString::from)
                        .collect(),
                    cwd: cwd.to_path_buf(),
                    timeout: remaining,
                    output_limit: MAX_DYNAMIC_OUTPUT_BYTES,
                    path: context.path.to_os_string(),
                    home: context.home_directory.map(Path::to_path_buf),
                })?;
                let remote_names = parse_git_remotes(&remotes)
                    .map_err(|_error| ())?
                    .into_iter()
                    .map(|item| item.value)
                    .collect::<Vec<_>>();
                let remote_names = remote_names.iter().map(String::as_str).collect::<Vec<_>>();
                parse_git_branches(
                    &output,
                    GitBranchOptions {
                        filter_active_branch: settings.filter_active_branch,
                        deduplicate_branches: settings.deduplicate_branches,
                        remote_names: &remote_names,
                        ..GitBranchOptions::default()
                    },
                )
                .map_err(|_error| ())
            }
            GeneratorSource::File { snapshot, parser } => parser.parse(&snapshot),
            GeneratorSource::Ssh {
                known_hosts,
                config,
            } => parse_ssh_hosts(
                known_hosts
                    .as_ref()
                    .map_or(&[], |file| file.bytes.as_slice()),
                config.as_ref().map_or(&[], |file| file.bytes.as_slice()),
            )
            .map_err(|_error| ()),
            GeneratorSource::Environment(names) => Ok(environment_variable_items(names)),
            GeneratorSource::FileTypes(cwd) => {
                let deadline = Instant::now().checked_add(spec.timeout).ok_or(())?;
                file_type_items(&cwd, cancellation, deadline)
            }
        }
    }
}

enum GeneratorSource {
    Process {
        binary: ResolvedBinary,
        arguments: Vec<OsString>,
        parser: ProcessParser,
    },
    File {
        snapshot: FileSnapshot,
        parser: FileParser,
    },
    Ssh {
        known_hosts: Option<FileSnapshot>,
        config: Option<FileSnapshot>,
    },
    Environment(Vec<String>),
    FileTypes(PathBuf),
}

#[derive(Clone, Copy)]
enum ProcessParser {
    GitBranches(GitGeneratorSettings),
    GitRemotes,
    GitTags,
    GitStashes,
    GitCommits,
    GitFiles,
    Zoxide,
    Processes,
    Services,
    Resource(DynamicResourceKind),
}

impl ProcessParser {
    fn parse(self, output: &[u8]) -> Result<Vec<DynamicItem>, ()> {
        match self {
            Self::GitBranches(settings) => parse_git_branches(
                output,
                GitBranchOptions {
                    filter_active_branch: settings.filter_active_branch,
                    deduplicate_branches: settings.deduplicate_branches,
                    ..GitBranchOptions::default()
                },
            ),
            Self::GitRemotes => parse_git_remotes(output),
            Self::GitTags => parse_git_tags(output),
            Self::GitStashes => parse_git_stashes(output),
            Self::GitCommits => parse_git_commits(output),
            Self::GitFiles => parse_nul_resources(output, DynamicResourceKind::GitFile),
            Self::Zoxide => parse_zoxide_directories(output),
            Self::Processes => parse_processes(output),
            Self::Services => parse_service_rows(output),
            Self::Resource(resource) => parse_resource_lines(output, resource),
        }
        .map_err(|_error| ())
    }
}

#[derive(Clone, Copy)]
enum FileParser {
    PackageScripts,
    MakeTargets,
    JustRecipes,
    NodePackages,
    CargoPackages,
    PythonPackages,
    GoPackages,
}

impl FileParser {
    fn parse(self, snapshot: &FileSnapshot) -> Result<Vec<DynamicItem>, ()> {
        match self {
            Self::PackageScripts => parse_package_scripts(&snapshot.bytes),
            Self::MakeTargets => parse_makefile_targets(&snapshot.bytes),
            Self::JustRecipes => parse_justfile_recipes(&snapshot.bytes),
            Self::NodePackages => parse_node_packages(&snapshot.bytes),
            Self::CargoPackages => parse_cargo_packages(&snapshot.bytes),
            Self::PythonPackages => parse_python_packages(&snapshot.path, &snapshot.bytes),
            Self::GoPackages => parse_go_packages(&snapshot.bytes),
        }
    }
}

struct ProcessPlan {
    program: PathBuf,
    arguments: Vec<OsString>,
    cwd: PathBuf,
    timeout: Duration,
    output_limit: usize,
    path: OsString,
    home: Option<PathBuf>,
}

trait CommandRunner {
    fn run(&mut self, plan: &ProcessPlan) -> Result<Vec<u8>, ()>;
}

struct NativeCommandRunner;

impl CommandRunner for NativeCommandRunner {
    fn run(&mut self, plan: &ProcessPlan) -> Result<Vec<u8>, ()> {
        let mut environment = vec![
            (OsString::from("PATH"), Some(plan.path.clone())),
            (OsString::from("LC_ALL"), Some(OsString::from("C"))),
            (OsString::from("TERM"), Some(OsString::from("dumb"))),
            (OsString::from("NO_COLOR"), Some(OsString::from("1"))),
            (OsString::from("CLICOLOR"), Some(OsString::from("0"))),
            (
                OsString::from("GIT_OPTIONAL_LOCKS"),
                Some(OsString::from("0")),
            ),
        ];
        if let Some(home) = &plan.home {
            environment.push((
                OsString::from("HOME"),
                Some(home.as_os_str().to_os_string()),
            ));
        }
        let request = LocalProcessRequest::new(
            plan.program.as_os_str(),
            plan.arguments.clone(),
            &plan.cwd,
            plan.timeout,
            plan.output_limit,
        )
        .and_then(|request| request.with_environment_overrides(environment))
        .map_err(|_error| ())?;
        let output = run_local_process(&request).map_err(|_error| ())?;
        if !output.exit().success() {
            return Err(());
        }
        Ok(output.into_stdout())
    }
}

#[derive(Clone)]
struct FileSnapshot {
    path: PathBuf,
    bytes: Vec<u8>,
    modified: SystemTime,
    length: u64,
}

impl FileSnapshot {
    fn cache_identity(&self) -> Vec<String> {
        vec![
            format!("file-path:{}", encoded_path(&self.path)),
            format!("file-mtime:{}", system_time_identity(self.modified)),
            format!("file-length:{}", self.length),
        ]
    }
}

fn read_optional_snapshot(path: &Path) -> Result<Option<FileSnapshot>, ()> {
    let path_metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(_error) => return Err(()),
    };
    if !path_metadata.is_file() {
        return Err(());
    }

    let mut file = match open_snapshot_file(path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(_error) => return Err(()),
    };
    let before = file.metadata().map_err(|_error| ())?;
    if !before.is_file() || before.len() > MAX_GENERATOR_FILE_BYTES as u64 {
        return Err(());
    }
    let mut bytes = Vec::with_capacity(usize::try_from(before.len()).unwrap_or(0));
    file.by_ref()
        .take((MAX_GENERATOR_FILE_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|_error| ())?;
    if bytes.len() > MAX_GENERATOR_FILE_BYTES {
        return Err(());
    }
    let after = file.metadata().map_err(|_error| ())?;
    if !same_file_snapshot(&before, &after) || after.len() != bytes.len() as u64 {
        return Err(());
    }
    Ok(Some(FileSnapshot {
        path: path.to_path_buf(),
        bytes,
        modified: after.modified().map_err(|_error| ())?,
        length: after.len(),
    }))
}

#[cfg(unix)]
fn open_snapshot_file(path: &Path) -> io::Result<File> {
    rustix::fs::open(
        path,
        rustix::fs::OFlags::RDONLY
            | rustix::fs::OFlags::CLOEXEC
            | rustix::fs::OFlags::NOFOLLOW
            | rustix::fs::OFlags::NONBLOCK,
        rustix::fs::Mode::empty(),
    )
    .map(File::from)
    .map_err(|error| io::Error::from_raw_os_error(error.raw_os_error()))
}

#[cfg(not(unix))]
fn open_snapshot_file(path: &Path) -> io::Result<File> {
    File::open(path)
}

fn same_file_snapshot(before: &Metadata, after: &Metadata) -> bool {
    before.len() == after.len()
        && before.modified().ok() == after.modified().ok()
        && file_identity(before) == file_identity(after)
}

#[cfg(unix)]
fn file_identity(metadata: &Metadata) -> (u64, u64) {
    use std::os::unix::fs::MetadataExt as _;
    (metadata.dev(), metadata.ino())
}

#[cfg(not(unix))]
fn file_identity(metadata: &Metadata) -> (u64, u64) {
    (metadata.len(), 0)
}

#[cfg(unix)]
fn is_executable(metadata: &Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt as _;
    metadata.permissions().mode() & 0o111 != 0
}

#[cfg(not(unix))]
fn is_executable(_metadata: &Metadata) -> bool {
    false
}

fn system_time_identity(time: SystemTime) -> String {
    match time.duration_since(SystemTime::UNIX_EPOCH) {
        Ok(duration) => format!("{}:{}", duration.as_secs(), duration.subsec_nanos()),
        Err(error) => {
            let duration = error.duration();
            format!("-{}:{}", duration.as_secs(), duration.subsec_nanos())
        }
    }
}

fn encoded_path(path: &Path) -> String {
    let bytes = path.as_os_str().as_encoded_bytes();
    let mut encoded = String::with_capacity(bytes.len().saturating_mul(2).min(MAX_IDENTITY_BYTES));
    for byte in bytes.iter().take(MAX_IDENTITY_BYTES / 2) {
        use std::fmt::Write as _;
        let _write = write!(encoded, "{byte:02x}");
    }
    encoded
}

fn directory_identity(path: &Path) -> Option<String> {
    let metadata = fs::metadata(path).ok()?;
    let modified = metadata.modified().ok()?;
    Some(format!(
        "directory:{}:{}",
        encoded_path(path),
        system_time_identity(modified)
    ))
}

fn parse_nul_resources(
    output: &[u8],
    resource: DynamicResourceKind,
) -> Result<Vec<DynamicItem>, super::DynamicParseError> {
    if output.len() > MAX_DYNAMIC_OUTPUT_BYTES {
        return parse_resource_lines(output, resource);
    }
    let mut framed = Vec::with_capacity(output.len());
    for value in output
        .split(|byte| *byte == 0)
        .filter(|value| !value.is_empty())
    {
        let value =
            std::str::from_utf8(value).map_err(|_| super::DynamicParseError::InvalidUtf8)?;
        if value.chars().any(char::is_control) {
            continue;
        }
        if framed.len().saturating_add(value.len() + 1) > MAX_DYNAMIC_OUTPUT_BYTES {
            break;
        }
        framed.extend_from_slice(value.as_bytes());
        framed.push(b'\n');
    }
    parse_resource_lines(&framed, resource)
}

fn parse_service_rows(output: &[u8]) -> Result<Vec<DynamicItem>, super::DynamicParseError> {
    if output.len() > MAX_DYNAMIC_OUTPUT_BYTES {
        return parse_resource_lines(output, DynamicResourceKind::Service);
    }
    let text = std::str::from_utf8(output).map_err(|_| super::DynamicParseError::InvalidUtf8)?;
    let values = text
        .lines()
        .filter_map(|line| line.split_ascii_whitespace().next())
        .map(str::to_owned);
    resource_items(values, DynamicResourceKind::Service)
}

fn parse_package_scripts(bytes: &[u8]) -> Result<Vec<DynamicItem>, ()> {
    let value: JsonValue = serde_json::from_slice(bytes).map_err(|_error| ())?;
    let scripts = value
        .get("scripts")
        .and_then(JsonValue::as_object)
        .ok_or(())?;
    resource_items(scripts.keys().cloned(), DynamicResourceKind::PackageScript).map_err(|_error| ())
}

fn parse_node_packages(bytes: &[u8]) -> Result<Vec<DynamicItem>, ()> {
    let value: JsonValue = serde_json::from_slice(bytes).map_err(|_error| ())?;
    let mut packages = BTreeSet::new();
    for table in [
        "dependencies",
        "devDependencies",
        "optionalDependencies",
        "peerDependencies",
    ] {
        if let Some(values) = value.get(table).and_then(JsonValue::as_object) {
            packages.extend(values.keys().take(MAX_MANIFEST_VALUES).cloned());
        }
    }
    resource_items(packages, DynamicResourceKind::Package).map_err(|_error| ())
}

fn parse_cargo_packages(bytes: &[u8]) -> Result<Vec<DynamicItem>, ()> {
    let text = std::str::from_utf8(bytes).map_err(|_error| ())?;
    let value: toml::Value = toml::from_str(text).map_err(|_error| ())?;
    let mut packages = BTreeSet::new();
    for path in [
        &["dependencies"][..],
        &["dev-dependencies"][..],
        &["build-dependencies"][..],
        &["workspace", "dependencies"][..],
    ] {
        if let Some(table) = toml_table_at(&value, path) {
            packages.extend(table.keys().take(MAX_MANIFEST_VALUES).cloned());
        }
    }
    resource_items(packages, DynamicResourceKind::Package).map_err(|_error| ())
}

fn parse_python_packages(path: &Path, bytes: &[u8]) -> Result<Vec<DynamicItem>, ()> {
    let text = std::str::from_utf8(bytes).map_err(|_error| ())?;
    let mut packages = BTreeSet::new();
    if path.file_name() == Some(OsStr::new("requirements.txt")) {
        packages.extend(text.lines().filter_map(python_requirement_name));
    } else {
        let value: toml::Value = toml::from_str(text).map_err(|_error| ())?;
        if let Some(dependencies) = value
            .get("project")
            .and_then(|project| project.get("dependencies"))
            .and_then(toml::Value::as_array)
        {
            packages.extend(
                dependencies
                    .iter()
                    .filter_map(toml::Value::as_str)
                    .filter_map(python_requirement_name),
            );
        }
        if let Some(table) = toml_table_at(&value, &["tool", "poetry", "dependencies"]) {
            packages.extend(
                table
                    .keys()
                    .filter(|name| name.as_str() != "python")
                    .cloned(),
            );
        }
    }
    resource_items(packages, DynamicResourceKind::Package).map_err(|_error| ())
}

fn python_requirement_name(line: &str) -> Option<String> {
    let line = line.trim();
    if line.is_empty() || line.starts_with(['#', '-']) || line.contains(" @ ") {
        return None;
    }
    let end = line
        .find(['<', '>', '=', '!', '~', ';', '['])
        .unwrap_or(line.len());
    let name = line[..end].trim();
    (!name.is_empty()).then(|| name.to_owned())
}

fn parse_go_packages(bytes: &[u8]) -> Result<Vec<DynamicItem>, ()> {
    let text = std::str::from_utf8(bytes).map_err(|_error| ())?;
    let mut packages = BTreeSet::new();
    let mut in_require = false;
    for raw in text.lines() {
        let line = raw
            .split_once("//")
            .map_or(raw, |(before, _comment)| before)
            .trim();
        if line == "require (" {
            in_require = true;
            continue;
        }
        if in_require && line == ")" {
            in_require = false;
            continue;
        }
        let fields = if in_require {
            Some(line)
        } else {
            line.strip_prefix("require ")
        };
        if let Some(name) = fields.and_then(|fields| fields.split_ascii_whitespace().next()) {
            packages.insert(name.to_owned());
        }
    }
    resource_items(packages, DynamicResourceKind::Package).map_err(|_error| ())
}

fn toml_table_at<'a>(
    value: &'a toml::Value,
    path: &[&str],
) -> Option<&'a toml::map::Map<String, toml::Value>> {
    let mut value = value;
    for component in path {
        value = value.get(*component)?;
    }
    value.as_table()
}

fn parse_makefile_targets(bytes: &[u8]) -> Result<Vec<DynamicItem>, ()> {
    let text = std::str::from_utf8(bytes).map_err(|_error| ())?;
    let mut targets = BTreeSet::new();
    let mut continuation = false;
    let mut define_depth = 0_u16;
    for raw in text.lines().take(MAX_DIRECT_FILES) {
        let trimmed = raw.trim_start();
        if trimmed.starts_with("define ") {
            define_depth = define_depth.saturating_add(1);
            continue;
        }
        if define_depth > 0 {
            if trimmed == "endef" {
                define_depth -= 1;
            }
            continue;
        }
        let continued = raw.trim_end().ends_with('\\');
        if continuation || raw.starts_with(char::is_whitespace) {
            continuation = continued;
            continue;
        }
        continuation = continued;
        if continued {
            continue;
        }
        let line = raw.split_once('#').map_or(raw, |(before, _comment)| before);
        let (left, right) = line
            .split_once("::")
            .or_else(|| line.split_once(':'))
            .unwrap_or(("", ""));
        if left.is_empty() {
            continue;
        }
        if right.starts_with('=') || right.contains(':') || left.contains(['=', '$', '%', '\\']) {
            continue;
        }
        targets.extend(left.split_ascii_whitespace().map(str::to_owned));
    }
    let mut database = String::from("# Files\n");
    for target in targets.into_iter().take(MAX_DYNAMIC_ITEMS) {
        database.push_str(&target);
        database.push_str(":\n");
    }
    database.push_str("# files hash-table stats:\n");
    parse_make_targets(database.as_bytes()).map_err(|_error| ())
}

fn parse_justfile_recipes(bytes: &[u8]) -> Result<Vec<DynamicItem>, ()> {
    let text = std::str::from_utf8(bytes).map_err(|_error| ())?;
    let mut recipes = BTreeSet::new();
    for raw in text.lines().take(MAX_DIRECT_FILES) {
        if raw.starts_with(char::is_whitespace) {
            continue;
        }
        let line = raw
            .split_once('#')
            .map_or(raw, |(before, _comment)| before)
            .trim();
        if line.contains(":=") {
            continue;
        }
        let Some((header, _body)) = line.split_once(':') else {
            continue;
        };
        if header.starts_with("alias ") {
            continue;
        }
        let name = header
            .split_ascii_whitespace()
            .find(|field| !field.starts_with('['))
            .unwrap_or("")
            .trim_start_matches('@');
        if !name.is_empty() {
            recipes.insert(name.to_owned());
        }
    }
    let summary = recipes.into_iter().collect::<Vec<_>>().join(" ");
    parse_just_recipes(summary.as_bytes()).map_err(|_error| ())
}

fn file_type_items(
    cwd: &Path,
    cancellation: &CancellationToken,
    deadline: Instant,
) -> Result<Vec<DynamicItem>, ()> {
    if cancellation.is_cancelled() || Instant::now() >= deadline {
        return Err(());
    }
    let entries = fs::read_dir(cwd).map_err(|_error| ())?;
    let mut extensions = BTreeSet::new();
    for entry in entries.take(MAX_DIRECT_FILES) {
        if cancellation.is_cancelled() || Instant::now() >= deadline {
            return Err(());
        }
        let Ok(entry) = entry else {
            continue;
        };
        let Ok(metadata) = entry.metadata() else {
            continue;
        };
        if cancellation.is_cancelled() || Instant::now() >= deadline {
            return Err(());
        }
        if !metadata.is_file() {
            continue;
        }
        if let Some(extension) = entry.path().extension().and_then(OsStr::to_str) {
            extensions.insert(extension.to_owned());
        }
    }
    if cancellation.is_cancelled() || Instant::now() >= deadline {
        return Err(());
    }
    resource_items(extensions, DynamicResourceKind::FileType).map_err(|_error| ())
}

fn resource_items(
    values: impl IntoIterator<Item = String>,
    resource: DynamicResourceKind,
) -> Result<Vec<DynamicItem>, super::DynamicParseError> {
    let mut framed = Vec::new();
    for value in values.into_iter().take(MAX_MANIFEST_VALUES) {
        if value.is_empty() || value.chars().any(char::is_control) {
            continue;
        }
        if framed.len().saturating_add(value.len() + 1) > MAX_DYNAMIC_OUTPUT_BYTES {
            break;
        }
        framed.extend_from_slice(value.as_bytes());
        framed.push(b'\n');
    }
    parse_resource_lines(&framed, resource)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    #[cfg(unix)]
    use std::os::unix::fs::{PermissionsExt as _, symlink};

    #[cfg(unix)]
    use nix::sys::stat::Mode;
    #[cfg(unix)]
    use nix::unistd::mkfifo;

    use crate::completion::{CommandSpec, FilesystemGenerator, GeneratorTarget, OptionSpec};

    use super::*;

    struct FakeRunner {
        responses: Vec<Result<Vec<u8>, ()>>,
        plans: Vec<(PathBuf, Vec<OsString>, Duration, usize, PathBuf)>,
    }

    impl FakeRunner {
        fn one(response: Result<Vec<u8>, ()>) -> Self {
            Self {
                responses: vec![response],
                plans: Vec::new(),
            }
        }
    }

    impl CommandRunner for FakeRunner {
        fn run(&mut self, plan: &ProcessPlan) -> Result<Vec<u8>, ()> {
            self.plans.push((
                plan.program.clone(),
                plan.arguments.clone(),
                plan.timeout,
                plan.output_limit,
                plan.cwd.clone(),
            ));
            self.responses.remove(0)
        }
    }

    struct CancellingRunner {
        state: Arc<AtomicBool>,
    }

    impl CommandRunner for CancellingRunner {
        fn run(&mut self, _plan: &ProcessPlan) -> Result<Vec<u8>, ()> {
            self.state.store(true, Ordering::Release);
            Ok(b"troy\n".to_vec())
        }
    }

    fn cancellation(cancelled: bool) -> CancellationToken {
        CancellationToken::observe(Arc::new(AtomicBool::new(cancelled)))
    }

    fn context<'a>(path: &'a OsStr, home: Option<&'a Path>) -> GeneratorExecutionContext<'a> {
        GeneratorExecutionContext {
            home_directory: home,
            ..GeneratorExecutionContext::new(ShellKind::Bash, path)
        }
    }

    fn temp_executable(directory: &Path, name: &str) -> PathBuf {
        let path = directory.join(name);
        fs::write(&path, "#!/bin/sh\nexit 99\n").unwrap();
        #[cfg(unix)]
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
        path
    }

    fn dynamic_index(name: &str, kind: GeneratorKind) -> SpecIndex {
        SpecIndex::new([CommandSpec::new(name, "test")
            .with_generator(GeneratorSpec::new(kind, GeneratorTarget::Positional(0)))])
        .unwrap()
    }

    #[test]
    fn every_git_generator_neutralizes_repository_supplied_configuration() {
        let kinds = [
            GeneratorKind::GitBranches,
            GeneratorKind::GitRemotes,
            GeneratorKind::GitTags,
            GeneratorKind::GitStashes,
            GeneratorKind::GitCommits,
            GeneratorKind::GitFiles,
        ];
        for kind in kinds {
            let label = format!("{kind:?}");
            let temporary = tempfile::tempdir().unwrap();
            temp_executable(temporary.path(), "git");
            let index = dynamic_index("git", kind);
            let query = CompletionQuery::new("git ", 4, temporary.path(), 1).unwrap();
            let mut executor = DynamicExecutor::new();
            let mut runner = FakeRunner {
                responses: vec![Ok(Vec::new()); 2],
                plans: Vec::new(),
            };
            executor.complete_curated_with(
                &index,
                &query,
                context(temporary.path().as_os_str(), None),
                &cancellation(false),
                &mut runner,
            );

            assert!(!runner.plans.is_empty(), "{label} ran no process");
            let required = GIT_READ_ONLY_CONFIG
                .map(OsString::from)
                .into_iter()
                .collect::<Vec<_>>();
            for plan in &runner.plans {
                assert!(
                    plan.1.starts_with(&required),
                    "{label} argv {:?} omits the neutralizing configuration",
                    plan.1
                );
            }
        }
    }

    #[test]
    fn curated_roots_and_unsafe_names_block_cobra_inference() {
        let index =
            SpecIndex::new([CommandSpec::new("kubectl", "curated").with_alias("k")]).unwrap();
        assert_eq!(
            cobra_inference_eligibility(&index, "KUBECTL"),
            CobraInferenceEligibility::CuratedDefinition
        );
        assert_eq!(
            cobra_inference_eligibility(&index, "k"),
            CobraInferenceEligibility::CuratedDefinition
        );
        assert_eq!(
            cobra_inference_eligibility(&index, "bin/kubectl"),
            CobraInferenceEligibility::InvalidExecutable
        );
        assert_eq!(
            cobra_inference_eligibility(&index, "communityctl"),
            CobraInferenceEligibility::Eligible
        );
    }

    #[test]
    fn executor_context_debug_redacts_paths_names_and_cached_values() {
        let names = vec!["ARGMAX_PRIVATE_NAME".to_owned()];
        let context = GeneratorExecutionContext {
            environment_names: &names,
            home_directory: Some(Path::new("/private/Troy-secret")),
            ..GeneratorExecutionContext::new(
                ShellKind::Bash,
                OsStr::new("/private/Abed-secret/bin"),
            )
        };
        let context_debug = format!("{context:?}");
        assert!(!context_debug.contains("Troy"));
        assert!(!context_debug.contains("Abed"));
        assert!(!context_debug.contains("ARGMAX_PRIVATE_NAME"));
        let executor_debug = format!("{:?}", DynamicExecutor::new());
        assert!(!executor_debug.contains("Troy"));
    }

    #[test]
    fn package_scripts_are_read_directly_and_never_execute_manifest_text() {
        let temporary = tempfile::tempdir().unwrap();
        let marker = temporary.path().join("side-effect");
        let document = format!(
            r#"{{"scripts":{{"study":"touch {}","test":"exit 42"}}}}"#,
            marker.display()
        );
        fs::write(temporary.path().join("package.json"), document).unwrap();
        let index = dynamic_index("npm", GeneratorKind::PackageScripts);
        let query = CompletionQuery::new("npm ", 4, temporary.path(), 1).unwrap();
        let mut executor = DynamicExecutor::new();
        let mut runner = FakeRunner::one(Err(()));
        let suggestions = executor.complete_curated_with(
            &index,
            &query,
            context(OsStr::new(""), Some(temporary.path())),
            &cancellation(false),
            &mut runner,
        );
        assert_eq!(
            suggestions
                .iter()
                .map(Suggestion::display)
                .collect::<Vec<_>>(),
            ["study", "test"]
        );
        assert!(runner.plans.is_empty());
        assert!(!marker.exists());
    }

    #[test]
    fn system_package_generators_use_fixed_non_shell_queries() {
        let temporary = tempfile::tempdir().unwrap();
        for executable in ["pacman", "yay", "paru", "rpm"] {
            temp_executable(temporary.path(), executable);
        }

        for (root, executable, arguments) in [
            ("pacman", "pacman", vec![OsString::from("-Qq")]),
            ("yay", "yay", vec![OsString::from("-Qq")]),
            ("paru", "paru", vec![OsString::from("-Qq")]),
            (
                "dnf",
                "rpm",
                ["-qa", "--qf", "%{NAME}\n"].map(OsString::from).into(),
            ),
            (
                "yum",
                "rpm",
                ["-qa", "--qf", "%{NAME}\n"].map(OsString::from).into(),
            ),
        ] {
            let index = dynamic_index(root, GeneratorKind::Packages);
            let line = format!("{root} ");
            let query = CompletionQuery::new(&line, line.len(), temporary.path(), 1).unwrap();
            let mut executor = DynamicExecutor::new();
            let mut runner = FakeRunner::one(Ok(b"troy\nabed\n".to_vec()));
            let suggestions = executor.complete_curated_with(
                &index,
                &query,
                context(temporary.path().as_os_str(), None),
                &cancellation(false),
                &mut runner,
            );

            assert_eq!(
                suggestions
                    .iter()
                    .map(Suggestion::display)
                    .collect::<Vec<_>>(),
                ["abed", "troy"]
            );
            assert_eq!(runner.plans.len(), 1);
            assert_eq!(runner.plans[0].0, temporary.path().join(executable));
            assert_eq!(runner.plans[0].1, arguments);
        }
    }

    #[cfg(unix)]
    #[test]
    fn file_backed_generator_rejects_fifo_without_waiting_for_a_writer() {
        let temporary = tempfile::tempdir().unwrap();
        let fifo = temporary.path().join("package.json");
        mkfifo(&fifo, Mode::S_IRUSR | Mode::S_IWUSR).unwrap();

        let started = Instant::now();
        let opened = open_snapshot_file(&fifo).unwrap();
        assert!(!opened.metadata().unwrap().is_file());
        assert!(read_optional_snapshot(&fifo).is_err());
        assert!(started.elapsed() < Duration::from_secs(1));
    }

    #[cfg(unix)]
    #[test]
    fn file_backed_generator_rejects_symlinks_to_regular_and_special_files() {
        let temporary = tempfile::tempdir().unwrap();
        let regular = temporary.path().join("private-manifest");
        fs::write(&regular, r#"{"scripts":{"study":"secret"}}"#).unwrap();
        let regular_link = temporary.path().join("package.json");
        symlink(&regular, &regular_link).unwrap();

        let fifo = temporary.path().join("control-pipe");
        mkfifo(&fifo, Mode::S_IRUSR | Mode::S_IWUSR).unwrap();
        let fifo_link = temporary.path().join("Makefile");
        symlink(&fifo, &fifo_link).unwrap();

        let started = Instant::now();
        assert!(open_snapshot_file(&regular_link).is_err());
        assert!(read_optional_snapshot(&regular_link).is_err());
        assert!(open_snapshot_file(&fifo_link).is_err());
        assert!(read_optional_snapshot(&fifo_link).is_err());
        assert!(started.elapsed() < Duration::from_secs(1));
    }

    #[test]
    fn make_and_just_are_parsed_without_evaluating_embedded_shells() {
        let make = b"SIDE := $(shell touch victim)\nall test: dep\n\t@touch victim\n%.o: %.c\n";
        assert_eq!(
            parse_makefile_targets(make)
                .unwrap()
                .iter()
                .map(|item| item.value.as_str())
                .collect::<Vec<_>>(),
            ["all", "test"]
        );
        let just = b"value := `touch victim`\n@study person='Troy Barnes':\n  echo {{person}}\n";
        assert_eq!(
            parse_justfile_recipes(just)
                .unwrap()
                .iter()
                .map(|item| item.value.as_str())
                .collect::<Vec<_>>(),
            ["study"]
        );
    }

    #[test]
    fn git_uses_exact_read_only_argv_timeout_and_stdout_bound() {
        let temporary = tempfile::tempdir().unwrap();
        let binary = temp_executable(temporary.path(), "git");
        let index = dynamic_index("git", GeneratorKind::GitTags);
        let query = CompletionQuery::new("git tr", 6, temporary.path(), 1).unwrap();
        let mut executor = DynamicExecutor::new();
        let mut runner = FakeRunner::one(Ok(b"troy\nabed\n".to_vec()));
        let suggestions = executor.complete_curated_with(
            &index,
            &query,
            context(temporary.path().as_os_str(), None),
            &cancellation(false),
            &mut runner,
        );
        assert_eq!(suggestions.len(), 1);
        assert_eq!(suggestions[0].display(), "troy");
        assert_eq!(runner.plans.len(), 1);
        assert_eq!(runner.plans[0].0, binary);
        assert_eq!(
            runner.plans[0].1,
            git_arguments(&["tag", "--list"])
                .into_iter()
                .map(OsString::from)
                .collect::<Vec<_>>()
        );
        assert_eq!(runner.plans[0].2, Duration::from_millis(150));
        assert_eq!(runner.plans[0].3, MAX_DYNAMIC_OUTPUT_BYTES);
    }

    #[test]
    fn git_branch_lookup_uses_remote_names_within_one_total_budget() {
        let temporary = tempfile::tempdir().unwrap();
        temp_executable(temporary.path(), "git");
        let index = dynamic_index("git", GeneratorKind::GitBranches);
        let query = CompletionQuery::new("git f", 5, temporary.path(), 1).unwrap();
        let mut executor = DynamicExecutor::new();
        let mut runner = FakeRunner {
            responses: vec![
                Ok(b"refs/heads/main\t*\nrefs/remotes/team/core/main\t \nrefs/remotes/team/core/feature\t \n".to_vec()),
                Ok(b"team/core\n".to_vec()),
            ],
            plans: Vec::new(),
        };
        let suggestions = executor.complete_curated_with(
            &index,
            &query,
            context(temporary.path().as_os_str(), None),
            &cancellation(false),
            &mut runner,
        );
        assert_eq!(
            suggestions
                .iter()
                .map(Suggestion::display)
                .collect::<Vec<_>>(),
            ["team/core/feature"]
        );
        assert_eq!(runner.plans.len(), 2);
        assert_eq!(
            runner.plans[1].1,
            git_arguments(&["remote"])
                .into_iter()
                .map(OsString::from)
                .collect::<Vec<_>>()
        );
        assert!(runner.plans[1].2 <= Duration::from_millis(150));
    }

    #[test]
    fn git_branch_cache_identity_includes_each_filter_setting() {
        let temporary = tempfile::tempdir().unwrap();
        temp_executable(temporary.path(), "git");
        let index = dynamic_index("git", GeneratorKind::GitBranches);
        let query = CompletionQuery::new("git ", 4, temporary.path(), 1).unwrap();
        let branches =
            b"refs/heads/main\t*\nrefs/heads/feature\t \nrefs/remotes/origin/feature\t \n";
        let mut runner = FakeRunner {
            responses: (0..3)
                .flat_map(|_| [Ok(branches.to_vec()), Ok(b"origin\n".to_vec())])
                .collect(),
            plans: Vec::new(),
        };
        let mut executor = DynamicExecutor::new();
        let default_context = context(temporary.path().as_os_str(), None);
        let filtered = executor.complete_curated_with(
            &index,
            &query,
            default_context,
            &cancellation(false),
            &mut runner,
        );
        let keep_active = executor.complete_curated_with(
            &index,
            &query,
            GeneratorExecutionContext {
                git: GitGeneratorSettings {
                    filter_active_branch: false,
                    deduplicate_branches: true,
                },
                ..default_context
            },
            &cancellation(false),
            &mut runner,
        );
        let keep_remote_duplicate = executor.complete_curated_with(
            &index,
            &query,
            GeneratorExecutionContext {
                git: GitGeneratorSettings {
                    filter_active_branch: true,
                    deduplicate_branches: false,
                },
                ..default_context
            },
            &cancellation(false),
            &mut runner,
        );

        assert_eq!(
            filtered
                .iter()
                .map(Suggestion::display)
                .collect::<BTreeSet<_>>(),
            BTreeSet::from(["feature"])
        );
        assert_eq!(
            keep_active
                .iter()
                .map(Suggestion::display)
                .collect::<BTreeSet<_>>(),
            BTreeSet::from(["feature", "main"])
        );
        assert_eq!(
            keep_remote_duplicate
                .iter()
                .map(Suggestion::display)
                .collect::<BTreeSet<_>>(),
            BTreeSet::from(["feature", "origin/feature"])
        );
        assert_eq!(runner.plans.len(), 6);

        let cached = executor.complete_curated_with(
            &index,
            &query,
            default_context,
            &cancellation(false),
            &mut runner,
        );
        assert_eq!(cached, filtered);
        assert_eq!(runner.plans.len(), 6);
    }

    #[test]
    fn filters_before_applying_the_generator_result_limit() {
        let temporary = tempfile::tempdir().unwrap();
        temp_executable(temporary.path(), "git");
        let mut spec = GeneratorSpec::new(GeneratorKind::GitTags, GeneratorTarget::Positional(0));
        spec.max_results = 1;
        let index = SpecIndex::new([CommandSpec::new("git", "test").with_generator(spec)]).unwrap();
        let query = CompletionQuery::new("git z", 5, temporary.path(), 1).unwrap();
        let mut runner = FakeRunner::one(Ok(b"alpha\nzulu\n".to_vec()));
        let suggestions = DynamicExecutor::new().complete_curated_with(
            &index,
            &query,
            context(temporary.path().as_os_str(), None),
            &cancellation(false),
            &mut runner,
        );
        assert_eq!(suggestions[0].display(), "zulu");
    }

    #[test]
    fn filesystem_scan_honors_entry_limit_cancellation_and_deadline() {
        let temporary = tempfile::tempdir().unwrap();
        for name in ["abed", "troy"] {
            fs::write(temporary.path().join(name), name).unwrap();
        }
        let query = CompletionQuery::new("open ", 5, temporary.path(), 1).unwrap();
        let filesystem = FilesystemGenerator {
            max_entries: 1,
            ..FilesystemGenerator::default()
        };
        let index = dynamic_index("open", GeneratorKind::Filesystem(filesystem.clone()));
        let mut runner = FakeRunner {
            responses: Vec::new(),
            plans: Vec::new(),
        };
        let suggestions = DynamicExecutor::new().complete_curated_with(
            &index,
            &query,
            context(OsStr::new(""), None),
            &cancellation(false),
            &mut runner,
        );
        assert_eq!(suggestions.len(), 1);

        assert!(
            DynamicExecutor::new()
                .complete_curated_with(
                    &index,
                    &query,
                    context(OsStr::new(""), None),
                    &cancellation(true),
                    &mut runner,
                )
                .is_empty()
        );

        let mut expired = GeneratorSpec::new(
            GeneratorKind::Filesystem(filesystem),
            GeneratorTarget::Positional(0),
        );
        expired.timeout = Duration::from_nanos(1);
        let expired_index =
            SpecIndex::new([CommandSpec::new("open", "test").with_generator(expired)]).unwrap();
        assert!(
            DynamicExecutor::new()
                .complete_curated_with(
                    &expired_index,
                    &query,
                    context(OsStr::new(""), None),
                    &cancellation(false),
                    &mut runner,
                )
                .is_empty()
        );
        assert!(runner.plans.is_empty());
    }

    #[test]
    fn missing_timeout_malformed_oversize_and_non_utf8_fail_independently() {
        let temporary = tempfile::tempdir().unwrap();
        temp_executable(temporary.path(), "git");
        let index = dynamic_index("git", GeneratorKind::GitTags);
        let query = CompletionQuery::new("git ", 4, temporary.path(), 1).unwrap();
        for response in [
            Err(()),
            Ok(vec![b'a'; MAX_DYNAMIC_OUTPUT_BYTES + 1]),
            Ok(vec![0xff, b'\n']),
        ] {
            let mut executor = DynamicExecutor::new();
            let mut runner = FakeRunner::one(response);
            assert!(
                executor
                    .complete_curated_with(
                        &index,
                        &query,
                        context(temporary.path().as_os_str(), None),
                        &cancellation(false),
                        &mut runner,
                    )
                    .is_empty()
            );
        }
        let empty_path = temporary.path().join("missing");
        fs::create_dir(&empty_path).unwrap();
        let mut executor = DynamicExecutor::new();
        let mut runner = FakeRunner::one(Ok(b"troy\n".to_vec()));
        assert!(
            executor
                .complete_curated_with(
                    &index,
                    &query,
                    context(empty_path.as_os_str(), None),
                    &cancellation(false),
                    &mut runner,
                )
                .is_empty()
        );
        assert!(runner.plans.is_empty());
    }

    #[test]
    fn cancellation_prevents_process_start_and_result_publication() {
        let temporary = tempfile::tempdir().unwrap();
        temp_executable(temporary.path(), "git");
        let index = dynamic_index("git", GeneratorKind::GitTags);
        let query = CompletionQuery::new("git ", 4, temporary.path(), 1).unwrap();
        let mut executor = DynamicExecutor::new();
        let mut runner = FakeRunner::one(Ok(b"troy\n".to_vec()));
        assert!(
            executor
                .complete_curated_with(
                    &index,
                    &query,
                    context(temporary.path().as_os_str(), None),
                    &cancellation(true),
                    &mut runner,
                )
                .is_empty()
        );
        assert!(runner.plans.is_empty());

        let state = Arc::new(AtomicBool::new(false));
        let token = CancellationToken::observe(Arc::clone(&state));
        let mut runner = CancellingRunner { state };
        assert!(
            executor
                .complete_curated_with(
                    &index,
                    &query,
                    context(temporary.path().as_os_str(), None),
                    &token,
                    &mut runner,
                )
                .is_empty()
        );
    }

    #[test]
    fn successful_dynamic_results_cache_but_transient_failures_do_not() {
        let temporary = tempfile::tempdir().unwrap();
        temp_executable(temporary.path(), "git");
        let index = dynamic_index("git", GeneratorKind::GitTags);
        let query = CompletionQuery::new("git ", 4, temporary.path(), 1).unwrap();
        let mut executor = DynamicExecutor::new();
        let mut runner = FakeRunner {
            responses: vec![Ok(b"troy\n".to_vec()), Err(())],
            plans: Vec::new(),
        };
        let first = executor.complete_curated_with(
            &index,
            &query,
            context(temporary.path().as_os_str(), None),
            &cancellation(false),
            &mut runner,
        );
        let second = executor.complete_curated_with(
            &index,
            &query,
            context(temporary.path().as_os_str(), None),
            &cancellation(false),
            &mut runner,
        );
        assert_eq!(first, second);
        assert_eq!(runner.plans.len(), 1);

        let other = CompletionQuery::new("git a", 5, temporary.path(), 2).unwrap();
        assert!(
            executor
                .complete_curated_with(
                    &index,
                    &other,
                    context(temporary.path().as_os_str(), None),
                    &cancellation(false),
                    &mut runner,
                )
                .is_empty()
        );
        assert_eq!(runner.plans.len(), 2);
    }

    #[test]
    fn volatile_environment_names_are_not_served_from_cache() {
        let temporary = tempfile::tempdir().unwrap();
        let index = dynamic_index("envctl", GeneratorKind::EnvironmentVariables);
        let query = CompletionQuery::new("envctl ", 7, temporary.path(), 1).unwrap();
        let mut executor = DynamicExecutor::new();
        let mut runner = FakeRunner {
            responses: Vec::new(),
            plans: Vec::new(),
        };
        let first_names = vec!["TROY".to_owned()];
        let first = executor.complete_curated_with(
            &index,
            &query,
            GeneratorExecutionContext {
                environment_names: &first_names,
                ..context(OsStr::new(""), None)
            },
            &cancellation(false),
            &mut runner,
        );
        let second_names = vec!["ABED".to_owned()];
        let second = executor.complete_curated_with(
            &index,
            &query,
            GeneratorExecutionContext {
                environment_names: &second_names,
                ..context(OsStr::new(""), None)
            },
            &cancellation(false),
            &mut runner,
        );
        assert_eq!(first[0].display(), "TROY");
        assert_eq!(second[0].display(), "ABED");
        assert!(runner.plans.is_empty());
    }

    #[test]
    fn dynamic_option_values_replace_the_complete_token_suffix_and_quote_inertly() {
        let temporary = tempfile::tempdir().unwrap();
        temp_executable(temporary.path(), "zoxide");
        let root = CommandSpec::new("study", "test")
            .with_option(OptionSpec::new("--member", "member").takes_value(true))
            .with_generator(GeneratorSpec::new(
                GeneratorKind::ZoxideDirectories,
                GeneratorTarget::OptionValue("--member".to_owned()),
            ));
        let index = SpecIndex::new([root]).unwrap();
        let line = "study --member=/tmp/TrAILING";
        let cursor = "study --member=/tmp/Tr".len();
        let query = CompletionQuery::new(line, cursor, temporary.path(), 1).unwrap();
        let mut runner = FakeRunner::one(Ok(b"1\t/tmp/Troy Barnes\n".to_vec()));
        let suggestions = DynamicExecutor::new().complete_curated_with(
            &index,
            &query,
            context(temporary.path().as_os_str(), None),
            &cancellation(false),
            &mut runner,
        );
        assert_eq!(suggestions.len(), 1);
        assert_eq!(
            suggestions[0].resulting_line(&query).unwrap(),
            "study --member='/tmp/Troy Barnes' "
        );
    }

    #[test]
    fn cobra_passes_malicious_text_as_one_argument_without_a_shell() {
        let temporary = tempfile::tempdir().unwrap();
        let binary = temp_executable(temporary.path(), "communityctl");
        let index = SpecIndex::new([]).unwrap();
        let line = "communityctl get '$(touch victim)' tr";
        let query = CompletionQuery::new(line, line.len(), temporary.path(), 1).unwrap();
        let mut executor = DynamicExecutor::new();
        let mut runner = FakeRunner::one(Ok(b"troy\tstudent\n:2\n".to_vec()));
        let completion = executor
            .complete_cobra_with(
                &index,
                &query,
                context(temporary.path().as_os_str(), None),
                &cancellation(false),
                &mut runner,
            )
            .unwrap();
        assert_eq!(runner.plans[0].0, binary);
        assert_eq!(
            runner.plans[0].1,
            ["__complete", "get", "$(touch victim)", "tr"].map(OsString::from)
        );
        assert_eq!(runner.plans[0].2, COBRA_COMPLETION_TIMEOUT);
        assert_eq!(
            completion.suggestions[0].insertion(),
            InsertionBehavior::Exact
        );
        assert_eq!(completion.suggestions[0].description(), "student");
    }

    #[test]
    fn cobra_fills_only_bare_or_unresolved_curated_nodes() {
        let temporary = tempfile::tempdir().unwrap();
        temp_executable(temporary.path(), "communityctl");
        let query = CompletionQuery::new("communityctl tr", 15, temporary.path(), 1).unwrap();
        let context = context(temporary.path().as_os_str(), None);

        let bare = SpecIndex::new([CommandSpec::new("communityctl", "catalog identity")]).unwrap();
        let mut runner = FakeRunner::one(Ok(b"troy\n:0\n".to_vec()));
        let completion = DynamicExecutor::new()
            .complete_cobra_with(&bare, &query, context, &cancellation(false), &mut runner)
            .unwrap();
        assert_eq!(completion.suggestions[0].display(), "troy");

        let modeled = SpecIndex::new([CommandSpec::new("communityctl", "curated")
            .with_subcommand(CommandSpec::new("tree", "modeled subtree"))])
        .unwrap();
        let mut runner = FakeRunner::one(Ok(b"troy\n:0\n".to_vec()));
        assert!(
            DynamicExecutor::new()
                .complete_cobra_with(&modeled, &query, context, &cancellation(false), &mut runner,)
                .is_none()
        );
        assert!(runner.plans.is_empty());
    }

    #[test]
    fn cobra_caches_only_successful_parsed_identity_and_rejects_malformed_output() {
        let temporary = tempfile::tempdir().unwrap();
        temp_executable(temporary.path(), "communityctl");
        let index = SpecIndex::new([]).unwrap();
        let query = CompletionQuery::new("communityctl tr", 15, temporary.path(), 1).unwrap();
        let mut executor = DynamicExecutor::new();
        let mut malformed = FakeRunner::one(Ok(b"troy\nnot-a-directive\n".to_vec()));
        assert!(
            executor
                .complete_cobra_with(
                    &index,
                    &query,
                    context(temporary.path().as_os_str(), None),
                    &cancellation(false),
                    &mut malformed,
                )
                .is_none()
        );
        let mut valid = FakeRunner::one(Ok(b"troy\n:0\n".to_vec()));
        let first = executor
            .complete_cobra_with(
                &index,
                &query,
                context(temporary.path().as_os_str(), None),
                &cancellation(false),
                &mut valid,
            )
            .unwrap();
        assert!(!first.cache_hit);
        let second = executor
            .complete_cobra_with(
                &index,
                &query,
                context(temporary.path().as_os_str(), None),
                &cancellation(false),
                &mut FakeRunner::one(Err(())),
            )
            .unwrap();
        assert!(second.cache_hit);
    }

    #[test]
    fn cobra_cache_invalidates_on_binary_mtime_and_preserves_keep_order() {
        let temporary = tempfile::tempdir().unwrap();
        let binary = temp_executable(temporary.path(), "communityctl");
        let index = SpecIndex::new([]).unwrap();
        let query = CompletionQuery::new("communityctl ", 13, temporary.path(), 1).unwrap();
        let mut executor = DynamicExecutor::new();
        let mut first_runner = FakeRunner::one(Ok(b"zulu\nalpha\n:32\n".to_vec()));
        let first = executor
            .complete_cobra_with(
                &index,
                &query,
                context(temporary.path().as_os_str(), None),
                &cancellation(false),
                &mut first_runner,
            )
            .unwrap();
        assert_eq!(
            first
                .suggestions
                .iter()
                .map(Suggestion::display)
                .collect::<Vec<_>>(),
            ["zulu", "alpha"]
        );

        std::thread::sleep(Duration::from_millis(20));
        fs::write(&binary, "#!/bin/sh\nexit 98\n# changed\n").unwrap();
        let mut second_runner = FakeRunner::one(Ok(b"abed\n:0\n".to_vec()));
        let second = executor
            .complete_cobra_with(
                &index,
                &query,
                context(temporary.path().as_os_str(), None),
                &cancellation(false),
                &mut second_runner,
            )
            .unwrap();
        assert!(!second.cache_hit);
        assert_eq!(second.suggestions[0].display(), "abed");
        assert_eq!(second_runner.plans.len(), 1);
    }

    #[test]
    fn cobra_cache_isolated_by_working_directory() {
        let temporary = tempfile::tempdir().unwrap();
        temp_executable(temporary.path(), "communityctl");
        let first_cwd = temporary.path().join("greendale");
        let second_cwd = temporary.path().join("city-college");
        fs::create_dir(&first_cwd).unwrap();
        fs::create_dir(&second_cwd).unwrap();
        let index = SpecIndex::new([]).unwrap();
        let first_query = CompletionQuery::new("communityctl tr", 15, &first_cwd, 1).unwrap();
        let second_query = CompletionQuery::new("communityctl tr", 15, &second_cwd, 2).unwrap();
        let mut executor = DynamicExecutor::new();

        let first = executor
            .complete_cobra_with(
                &index,
                &first_query,
                context(temporary.path().as_os_str(), None),
                &cancellation(false),
                &mut FakeRunner::one(Ok(b"troy\n:0\n".to_vec())),
            )
            .unwrap();
        let mut second_runner = FakeRunner::one(Ok(b"abed\n:0\n".to_vec()));
        let second = executor
            .complete_cobra_with(
                &index,
                &second_query,
                context(temporary.path().as_os_str(), None),
                &cancellation(false),
                &mut second_runner,
            )
            .unwrap();

        assert!(!first.cache_hit);
        assert!(!second.cache_hit);
        assert_eq!(first.suggestions[0].display(), "troy");
        assert_eq!(second.suggestions[0].display(), "abed");
        assert_eq!(second_runner.plans[0].4, second_cwd);
    }

    #[test]
    fn direct_reads_are_bounded_and_reject_non_utf8() {
        let temporary = tempfile::tempdir().unwrap();
        let manifest = temporary.path().join("package.json");
        fs::write(&manifest, vec![b'x'; MAX_GENERATOR_FILE_BYTES + 1]).unwrap();
        assert!(read_optional_snapshot(&manifest).is_err());
        fs::write(&manifest, [0xff]).unwrap();
        let snapshot = read_optional_snapshot(&manifest).unwrap().unwrap();
        assert!(parse_package_scripts(&snapshot.bytes).is_err());
    }

    #[test]
    fn ssh_parser_reads_only_bounded_config_and_known_hosts() {
        let temporary = tempfile::tempdir().unwrap();
        let ssh = temporary.path().join(".ssh");
        fs::create_dir(&ssh).unwrap();
        fs::write(
            ssh.join("config"),
            "Host greendale *.invalid !negated\n  User troy\n",
        )
        .unwrap();
        fs::write(
            ssh.join("known_hosts"),
            "study-room ssh-ed25519 AAAA fake\n",
        )
        .unwrap();
        let prepared = DynamicExecutor::prepare_ssh(Some(temporary.path())).unwrap();
        let items = prepared
            .execute(
                &GeneratorSpec::new(GeneratorKind::SshHosts, GeneratorTarget::Positional(0)),
                temporary.path(),
                context(OsStr::new(""), Some(temporary.path())),
                &cancellation(false),
                &mut FakeRunner::one(Err(())),
            )
            .unwrap();
        assert_eq!(
            items
                .iter()
                .map(|item| item.value.as_str())
                .collect::<Vec<_>>(),
            ["greendale", "study-room"]
        );
    }
}
