//! Safe normalization of captured local-generator output.
//!
//! Parsers in this module consume already captured bytes. They neither construct
//! shell command strings nor execute anything, and their results remain inert
//! until a separate completion layer quotes and inserts them.

use std::collections::{BTreeMap, BTreeSet};
use std::error::Error;
use std::fmt;

/// Largest captured body accepted from one dynamic generator.
pub const MAX_DYNAMIC_OUTPUT_BYTES: usize = 256 * 1024;
/// Largest normalized value and description accepted for one item.
pub const MAX_DYNAMIC_ITEM_BYTES: usize = 4 * 1024;
/// Largest number of unique items returned by one parser.
pub const MAX_DYNAMIC_ITEMS: usize = 500;

const LONGEST_SSH_DESCRIPTION: &str = "SSH host from known_hosts (port 65535)";
const MAX_ENVIRONMENT_NAMES_INSPECTED: usize = 4_096;

/// Source-specific type of one normalized dynamic item.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum DynamicItemKind {
    /// Local or remote Git branch.
    GitBranch,
    /// Git remote.
    GitRemote,
    /// Git tag.
    GitTag,
    /// Git stash reference.
    GitStash,
    /// Recent Git commit.
    GitCommit,
    /// Make target.
    MakeTarget,
    /// Just recipe.
    JustRecipe,
    /// Concrete SSH host or alias.
    SshHost,
    /// Directory known to zoxide.
    ZoxideDirectory,
    /// Local process identifier.
    Process,
    /// Environment-variable name.
    EnvironmentVariable,
    /// Other bounded newline-oriented resource.
    Resource(DynamicResourceKind),
}

/// Supported newline-oriented resource types.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum DynamicResourceKind {
    /// File known to the active Git repository.
    GitFile,
    /// Docker container name or identifier.
    DockerContainer,
    /// Docker image name or identifier.
    DockerImage,
    /// Package name from a context-specific local command.
    Package,
    /// Script declared by local package-manager metadata.
    PackageScript,
    /// Local service or unit name.
    Service,
    /// File-type value.
    FileType,
}

impl DynamicResourceKind {
    const fn description(self) -> &'static str {
        match self {
            Self::GitFile => "Git file",
            Self::DockerContainer => "Docker container",
            Self::DockerImage => "Docker image",
            Self::Package => "package",
            Self::PackageScript => "package script",
            Self::Service => "service",
            Self::FileType => "file type",
        }
    }
}

/// Whether a branch comes from a local or remote ref namespace.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum GitBranchScope {
    /// Branch under `refs/heads`.
    Local,
    /// Branch under `refs/remotes`, including its remote name.
    Remote {
        /// Remote prefix, such as `origin`.
        remote: String,
    },
}

/// Source metadata preserved while an item remains inert.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DynamicMetadata {
    /// The source has no additional structured metadata.
    None,
    /// Git branch scope and active-worktree state.
    GitBranch {
        /// Local or remote ref namespace.
        scope: GitBranchScope,
        /// Whether this is the active branch in the current worktree.
        active: bool,
    },
    /// OpenSSH discovery sources for one concrete host.
    SshHost {
        /// The host appeared in a `known_hosts` file.
        known_hosts: bool,
        /// The host appeared in an SSH `Host` directive.
        config: bool,
        /// Port encoded by a `[host]:port` `known_hosts` pattern.
        port: Option<u16>,
    },
    /// Validated zoxide score, retained as exact decimal text.
    Zoxide {
        /// Finite, non-negative score emitted by zoxide.
        score: String,
    },
    /// Parsed process identifier.
    Process {
        /// Positive operating-system process identifier.
        pid: u32,
    },
}

/// One normalized, terminal-safe value produced from captured local output.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DynamicItem {
    /// Exact value offered to the later quoting and insertion layer.
    pub value: String,
    /// Short terminal-safe context shown beside the value.
    pub description: String,
    /// Source-specific type used for icons and ranking.
    pub kind: DynamicItemKind,
    /// Structured source details that must not be inferred from display text.
    pub metadata: DynamicMetadata,
}

/// Branch filtering supplied by the caller's Git settings and repository state.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct GitBranchOptions<'a> {
    /// Active branch name, when the repository is not detached.
    pub active_branch: Option<&'a str>,
    /// Hide the active local branch where selecting it would be a no-op.
    pub filter_active_branch: bool,
    /// Prefer a local branch over equivalent `<remote>/<branch>` rows.
    pub deduplicate_branches: bool,
    /// Known remote names used to disambiguate remotes that contain `/`.
    ///
    /// The longest matching name wins, so `foo/bar` is preferred over `foo`
    /// for `refs/remotes/foo/bar/main`. At most [`MAX_DYNAMIC_ITEMS`] entries
    /// are inspected.
    pub remote_names: &'a [&'a str],
}

impl Default for GitBranchOptions<'_> {
    fn default() -> Self {
        Self {
            active_branch: None,
            filter_active_branch: true,
            deduplicate_branches: true,
            remote_names: &[],
        }
    }
}

/// Failure that invalidates an entire captured generator response.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DynamicParseError {
    /// Captured bytes exceeded the hard input budget.
    OutputTooLarge {
        /// Observed body size in bytes.
        size: usize,
        /// Maximum accepted body size in bytes.
        limit: usize,
    },
    /// Captured bytes were not valid UTF-8.
    InvalidUtf8,
}

impl fmt::Display for DynamicParseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::OutputTooLarge { size, limit } => {
                write!(
                    formatter,
                    "dynamic output is {size} bytes; limit is {limit}"
                )
            }
            Self::InvalidUtf8 => formatter.write_str("dynamic output is not valid UTF-8"),
        }
    }
}

impl Error for DynamicParseError {}

/// Parses local and remote Git refs with configurable active-branch filtering.
///
/// Accepted rows are short branch names, `git branch` marker rows, or explicit
/// `refs/heads/...`, `refs/remotes/...`, and `remotes/...` names. A tab-separated
/// `*` field may also mark the active branch. Symbolic remote `HEAD` rows are
/// ignored. When deduplication is enabled, a local branch wins over every remote
/// row with the same branch suffix.
///
/// # Errors
///
/// Returns an error when `output` exceeds [`MAX_DYNAMIC_OUTPUT_BYTES`] or is not
/// valid UTF-8. Malformed individual rows are ignored.
pub fn parse_git_branches(
    output: &[u8],
    options: GitBranchOptions<'_>,
) -> Result<Vec<DynamicItem>, DynamicParseError> {
    let text = decode_output(output)?;
    let active_branch = options.active_branch.and_then(normalize_active_branch);
    let remote_names = normalize_remote_names(options.remote_names);
    let branches = output_lines(text)
        .filter_map(|line| parse_branch_line(line, &remote_names))
        .collect::<Vec<_>>();
    let mut local_activity = BTreeMap::new();

    for branch in &branches {
        if !matches!(branch.scope, GitBranchScope::Local) {
            continue;
        }
        let active = branch.active || active_branch == Some(branch.branch.as_str());
        local_activity
            .entry(branch.branch.clone())
            .and_modify(|known_active| *known_active |= active)
            .or_insert(active);
    }

    let local_names = local_activity.keys().cloned().collect::<BTreeSet<_>>();
    let mut items = BTreeMap::new();
    for (branch, active) in local_activity {
        if options.filter_active_branch && active {
            continue;
        }
        let Some(item) = branch_item(branch, GitBranchScope::Local, active) else {
            continue;
        };
        insert_bounded(&mut items, item.value.clone(), item);
    }

    for branch in branches {
        let GitBranchScope::Remote { remote } = branch.scope else {
            continue;
        };
        if options.deduplicate_branches && local_names.contains(&branch.branch) {
            continue;
        }
        let value = format!("{remote}/{}", branch.branch);
        let Some(item) = branch_item(value, GitBranchScope::Remote { remote }, false) else {
            continue;
        };
        insert_bounded(&mut items, item.value.clone(), item);
    }

    Ok(items.into_values().collect())
}

/// Parses newline-separated Git remote names in lexical order.
///
/// # Errors
///
/// Returns an error for excessive or invalid UTF-8 output. Unsafe rows are
/// ignored independently.
pub fn parse_git_remotes(output: &[u8]) -> Result<Vec<DynamicItem>, DynamicParseError> {
    parse_plain_lines(
        output,
        DynamicItemKind::GitRemote,
        "Git remote",
        valid_git_remote,
    )
}

/// Parses newline-separated Git tag names in lexical order.
///
/// # Errors
///
/// Returns an error for excessive or invalid UTF-8 output. Unsafe rows are
/// ignored independently.
pub fn parse_git_tags(output: &[u8]) -> Result<Vec<DynamicItem>, DynamicParseError> {
    parse_plain_lines(output, DynamicItemKind::GitTag, "Git tag", valid_git_ref)
}

/// Parses `stash-ref<TAB>summary` rows while preserving recency order.
///
/// # Errors
///
/// Returns an error for excessive or invalid UTF-8 output. Unsafe rows are
/// ignored independently.
pub fn parse_git_stashes(output: &[u8]) -> Result<Vec<DynamicItem>, DynamicParseError> {
    parse_git_described_rows(output, DynamicItemKind::GitStash, valid_stash_ref)
}

/// Parses `commit-id<TAB>subject` rows while preserving recency order.
///
/// # Errors
///
/// Returns an error for excessive or invalid UTF-8 output. Unsafe rows are
/// ignored independently.
pub fn parse_git_commits(output: &[u8]) -> Result<Vec<DynamicItem>, DynamicParseError> {
    parse_git_described_rows(output, DynamicItemKind::GitCommit, valid_object_id)
}

/// Discovers explicit targets from `LC_ALL=C make -qpRr` rule rows.
///
/// Only the final `# Files` section is considered, preventing multiline variable
/// bodies from spoofing rule rows. Special targets, pattern rules, variable
/// expressions, and assignment rows are ignored. Multiple explicit targets
/// before one colon are returned independently in lexical order.
///
/// # Errors
///
/// Returns an error for excessive or invalid UTF-8 output. Unsafe rows are
/// ignored independently.
pub fn parse_make_targets(output: &[u8]) -> Result<Vec<DynamicItem>, DynamicParseError> {
    let text = decode_output(output)?;
    let mut items = BTreeMap::new();
    let mut skip_not_target = false;
    let lines = output_lines(text).collect::<Vec<_>>();
    let Some(files_index) = lines.iter().rposition(|line| *line == "# Files") else {
        return Ok(Vec::new());
    };
    let mut section_complete = false;

    for line in &lines[files_index + 1..] {
        if *line == "# files hash-table stats:" || line.starts_with("# Finished Make data base") {
            section_complete = true;
            break;
        }
        if *line == "# Not a target:" {
            skip_not_target = true;
            continue;
        }
        if line.is_empty()
            || line.starts_with(char::is_whitespace)
            || line.starts_with('#')
            || contains_unsafe_control(line, true)
        {
            continue;
        }
        let Some((targets, prerequisites)) = line.split_once(':') else {
            continue;
        };
        if skip_not_target {
            skip_not_target = false;
            continue;
        }
        if prerequisites.trim_start().starts_with('=') {
            continue;
        }
        let targets = targets.trim_end().strip_suffix('&').unwrap_or(targets);
        if targets.contains('\\') {
            continue;
        }
        for target in targets.split_ascii_whitespace() {
            if valid_make_target(target) && valid_item_fields(target, "Make target") {
                let item = plain_item(target, "Make target", DynamicItemKind::MakeTarget);
                insert_bounded(&mut items, target.to_owned(), item);
            }
        }
    }

    if !section_complete {
        return Ok(Vec::new());
    }

    Ok(items.into_values().collect())
}

/// Parses whitespace-separated recipe names from `just --summary` output.
///
/// # Errors
///
/// Returns an error for excessive or invalid UTF-8 output. Unsafe recipe tokens
/// are ignored independently.
pub fn parse_just_recipes(output: &[u8]) -> Result<Vec<DynamicItem>, DynamicParseError> {
    let text = decode_output(output)?;
    let mut items = BTreeMap::new();

    for recipe in text.split_ascii_whitespace() {
        if !valid_just_recipe(recipe) || !valid_item_fields(recipe, "Just recipe") {
            continue;
        }
        let item = plain_item(recipe, "Just recipe", DynamicItemKind::JustRecipe);
        insert_bounded(&mut items, recipe.to_owned(), item);
    }

    Ok(items.into_values().collect())
}

/// Merges concrete hosts from OpenSSH `known_hosts` and config text.
///
/// Wildcard, negated, and hashed patterns are ignored. Comma-separated
/// `known_hosts` aliases and whitespace-separated `Host` patterns are expanded
/// without interpreting quotes, substitutions, or shell syntax. Host matching
/// for deduplication is case-insensitive.
///
/// # Errors
///
/// Returns an error when the combined input exceeds
/// [`MAX_DYNAMIC_OUTPUT_BYTES`] or either body is not valid UTF-8. Unsafe rows
/// are ignored independently.
pub fn parse_ssh_hosts(
    known_hosts: &[u8],
    config: &[u8],
) -> Result<Vec<DynamicItem>, DynamicParseError> {
    let size = known_hosts.len().saturating_add(config.len());
    if size > MAX_DYNAMIC_OUTPUT_BYTES {
        return Err(DynamicParseError::OutputTooLarge {
            size,
            limit: MAX_DYNAMIC_OUTPUT_BYTES,
        });
    }
    let known_hosts = decode_utf8(known_hosts)?;
    let config = decode_utf8(config)?;
    let mut hosts = BTreeMap::<String, ParsedSshHost>::new();

    parse_known_hosts(known_hosts, &mut hosts);
    parse_ssh_config(config, &mut hosts);

    Ok(hosts.into_values().map(ParsedSshHost::into_item).collect())
}

/// Parses zoxide `score<TAB>path` or `score path` rows.
///
/// Duplicate paths retain their highest finite, non-negative score. Results are
/// ordered by descending score and then lexical path.
///
/// # Errors
///
/// Returns an error for excessive or invalid UTF-8 output. Unsafe rows are
/// ignored independently.
pub fn parse_zoxide_directories(output: &[u8]) -> Result<Vec<DynamicItem>, DynamicParseError> {
    let text = decode_output(output)?;
    let mut directories = BTreeMap::<String, ScoredDirectory>::new();

    for line in output_lines(text) {
        let Some((score_text, path)) = split_score_path(line) else {
            continue;
        };
        let Ok(score) = score_text.parse::<f64>() else {
            continue;
        };
        let description = format!("zoxide score {score_text}");
        if !score.is_finite() || score.is_sign_negative() || !valid_item_fields(path, &description)
        {
            continue;
        }

        let candidate = ScoredDirectory {
            path: path.to_owned(),
            score,
            score_text: score_text.to_owned(),
        };
        match directories.get(path) {
            Some(current) if current.score >= score => {}
            Some(_) => {
                directories.insert(path.to_owned(), candidate);
            }
            None if directories.len() < MAX_DYNAMIC_ITEMS => {
                directories.insert(path.to_owned(), candidate);
            }
            None => {}
        }
    }

    let mut directories = directories.into_values().collect::<Vec<_>>();
    directories.sort_by(|left, right| {
        right
            .score
            .total_cmp(&left.score)
            .then_with(|| left.path.cmp(&right.path))
    });
    Ok(directories
        .into_iter()
        .map(ScoredDirectory::into_item)
        .collect())
}

/// Parses `pid process-name` rows, returning numeric PID order.
///
/// # Errors
///
/// Returns an error for excessive or invalid UTF-8 output. Unsafe rows are
/// ignored independently.
pub fn parse_processes(output: &[u8]) -> Result<Vec<DynamicItem>, DynamicParseError> {
    let text = decode_output(output)?;
    let mut processes = BTreeMap::new();

    for line in output_lines(text) {
        let line = line.trim();
        let Some(separator) = line.find(char::is_whitespace) else {
            continue;
        };
        let pid_text = &line[..separator];
        let name = line[separator..].trim();
        if !pid_text.bytes().all(|byte| byte.is_ascii_digit()) {
            continue;
        }
        let Ok(pid) = pid_text.parse::<u32>() else {
            continue;
        };
        let value = pid.to_string();
        if pid == 0 || !valid_item_fields(&value, name) {
            continue;
        }
        let item = DynamicItem {
            value,
            description: name.to_owned(),
            kind: DynamicItemKind::Process,
            metadata: DynamicMetadata::Process { pid },
        };
        insert_bounded(&mut processes, pid, item);
    }

    Ok(processes.into_values().collect())
}

/// Builds environment-variable items from a structured name snapshot.
///
/// Taking names separately prevents newline-bearing values from being mistaken
/// for additional variables. At most 4,096 input names are inspected and no
/// values are accepted or retained.
pub fn environment_variable_items<I, S>(names: I) -> Vec<DynamicItem>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let mut items = BTreeMap::new();

    for name in names.into_iter().take(MAX_ENVIRONMENT_NAMES_INSPECTED) {
        let name = name.as_ref();
        if !valid_environment_name(name) || !valid_item_fields(name, "environment variable") {
            continue;
        }
        let item = plain_item(
            name,
            "environment variable",
            DynamicItemKind::EnvironmentVariable,
        );
        insert_bounded(&mut items, name.to_owned(), item);
    }

    items.into_values().collect()
}

/// Parses newline-separated resource values with optional tab descriptions.
///
/// The first field is always the inert value. A non-empty second field replaces
/// the resource's static description; additional tabs make the row invalid.
///
/// # Errors
///
/// Returns an error for excessive or invalid UTF-8 output. Unsafe rows are
/// ignored independently.
pub fn parse_resource_lines(
    output: &[u8],
    resource: DynamicResourceKind,
) -> Result<Vec<DynamicItem>, DynamicParseError> {
    let text = decode_output(output)?;
    let mut items = BTreeMap::new();

    for line in output_lines(text) {
        let Some((value, detail)) = split_optional_description(line) else {
            continue;
        };
        let description = if detail.is_empty() {
            resource.description()
        } else {
            detail
        };
        if !valid_item_fields(value, description) {
            continue;
        }
        let item = DynamicItem {
            value: value.to_owned(),
            description: description.to_owned(),
            kind: DynamicItemKind::Resource(resource),
            metadata: DynamicMetadata::None,
        };
        insert_bounded(&mut items, value.to_owned(), item);
    }

    Ok(items.into_values().collect())
}

#[derive(Clone, Debug)]
struct ParsedBranch {
    branch: String,
    scope: GitBranchScope,
    active: bool,
}

fn parse_branch_line(line: &str, remote_names: &[&str]) -> Option<ParsedBranch> {
    if contains_unsafe_control(line, true) {
        return None;
    }
    let (reference, fields) = line.split_once('\t').map_or((line, ""), |parts| parts);
    let reference = reference.trim();
    let (reference, marker_active) = reference
        .strip_prefix("* ")
        .map_or((reference, false), |rest| (rest.trim_start(), true));
    let reference = reference
        .strip_prefix("+ ")
        .map_or(reference, str::trim_start);
    if reference.contains(" -> ") {
        return None;
    }
    let active = marker_active || fields.split('\t').any(|field| field.trim() == "*");

    let (branch, scope) = if let Some(remote_ref) = reference.strip_prefix("refs/remotes/") {
        parse_remote_branch(remote_ref, remote_names)?
    } else if let Some(remote_ref) = reference.strip_prefix("remotes/") {
        parse_remote_branch(remote_ref, remote_names)?
    } else {
        let branch = reference.strip_prefix("refs/heads/").unwrap_or(reference);
        if !valid_git_ref(branch) {
            return None;
        }
        (branch.to_owned(), GitBranchScope::Local)
    };

    Some(ParsedBranch {
        branch,
        scope,
        active,
    })
}

fn parse_remote_branch(reference: &str, remote_names: &[&str]) -> Option<(String, GitBranchScope)> {
    if !valid_git_ref(reference) {
        return None;
    }
    let configured = remote_names.iter().copied().find_map(|remote| {
        reference
            .strip_prefix(remote)
            .and_then(|suffix| suffix.strip_prefix('/'))
            .filter(|branch| valid_git_ref(branch))
            .map(|branch| (remote, branch))
    });
    let (remote, branch) = configured.or_else(|| reference.split_once('/'))?;
    if !valid_git_remote(remote)
        || !valid_git_ref(branch)
        || branch == "HEAD"
        || reference.ends_with("/HEAD")
    {
        return None;
    }
    Some((
        branch.to_owned(),
        GitBranchScope::Remote {
            remote: remote.to_owned(),
        },
    ))
}

fn normalize_remote_names<'a>(remote_names: &'a [&'a str]) -> Vec<&'a str> {
    let mut names = remote_names
        .iter()
        .take(MAX_DYNAMIC_ITEMS)
        .copied()
        .filter(|remote| valid_git_remote(remote))
        .collect::<Vec<_>>();
    names
        .sort_unstable_by(|left, right| right.len().cmp(&left.len()).then_with(|| left.cmp(right)));
    names.dedup();
    names
}

fn normalize_active_branch(branch: &str) -> Option<&str> {
    let branch = branch.strip_prefix("refs/heads/").unwrap_or(branch);
    valid_git_ref(branch).then_some(branch)
}

fn branch_item(value: String, scope: GitBranchScope, active: bool) -> Option<DynamicItem> {
    let description = match &scope {
        GitBranchScope::Local if active => "active Git branch".to_owned(),
        GitBranchScope::Local => "local Git branch".to_owned(),
        GitBranchScope::Remote { remote } => format!("Git branch on {remote}"),
    };
    if !valid_item_fields(&value, &description) {
        return None;
    }
    Some(DynamicItem {
        value,
        description,
        kind: DynamicItemKind::GitBranch,
        metadata: DynamicMetadata::GitBranch { scope, active },
    })
}

fn parse_plain_lines(
    output: &[u8],
    kind: DynamicItemKind,
    description: &'static str,
    validate: fn(&str) -> bool,
) -> Result<Vec<DynamicItem>, DynamicParseError> {
    let text = decode_output(output)?;
    let mut items = BTreeMap::new();
    for line in output_lines(text) {
        let value = line.trim();
        if !validate(value) || !valid_item_fields(value, description) {
            continue;
        }
        insert_bounded(
            &mut items,
            value.to_owned(),
            plain_item(value, description, kind),
        );
    }
    Ok(items.into_values().collect())
}

fn parse_git_described_rows(
    output: &[u8],
    kind: DynamicItemKind,
    validate: fn(&str) -> bool,
) -> Result<Vec<DynamicItem>, DynamicParseError> {
    let text = decode_output(output)?;
    let mut seen = BTreeSet::new();
    let mut items = Vec::new();

    for line in output_lines(text) {
        let Some((value, description)) = line.split_once('\t') else {
            continue;
        };
        if line.matches('\t').count() != 1 || !validate(value) {
            continue;
        }
        let description = if description.is_empty() {
            match kind {
                DynamicItemKind::GitStash => "Git stash",
                DynamicItemKind::GitCommit => "Git commit",
                _ => "Git reference",
            }
        } else {
            description
        };
        if !valid_item_fields(value, description) || !seen.insert(value.to_owned()) {
            continue;
        }
        items.push(DynamicItem {
            value: value.to_owned(),
            description: description.to_owned(),
            kind,
            metadata: DynamicMetadata::None,
        });
        if items.len() == MAX_DYNAMIC_ITEMS {
            break;
        }
    }
    Ok(items)
}

#[derive(Clone, Debug)]
struct ParsedSshHost {
    value: String,
    known_hosts: bool,
    config: bool,
    port: Option<u16>,
}

impl ParsedSshHost {
    fn into_item(self) -> DynamicItem {
        let source = match (self.config, self.known_hosts) {
            (true, true) => "SSH host from config and known_hosts".to_owned(),
            (true, false) => "SSH host from config".to_owned(),
            (false, true) => self.port.map_or_else(
                || "SSH host from known_hosts".to_owned(),
                |port| format!("SSH host from known_hosts (port {port})"),
            ),
            (false, false) => "SSH host".to_owned(),
        };
        DynamicItem {
            value: self.value,
            description: source,
            kind: DynamicItemKind::SshHost,
            metadata: DynamicMetadata::SshHost {
                known_hosts: self.known_hosts,
                config: self.config,
                port: self.port,
            },
        }
    }
}

fn parse_known_hosts(text: &str, hosts: &mut BTreeMap<String, ParsedSshHost>) {
    for line in output_lines(text) {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') || contains_unsafe_control(line, true) {
            continue;
        }
        let mut fields = line.split_ascii_whitespace();
        let Some(first) = fields.next() else {
            continue;
        };
        let patterns = if first.starts_with('@') {
            let Some(patterns) = fields.next() else {
                continue;
            };
            patterns
        } else {
            first
        };
        if fields.next().is_none() || fields.next().is_none() {
            continue;
        }
        for pattern in patterns.split(',') {
            let Some((host, port)) = concrete_known_host(pattern) else {
                continue;
            };
            merge_ssh_host(hosts, host, true, false, port);
        }
    }
}

fn parse_ssh_config(text: &str, hosts: &mut BTreeMap<String, ParsedSshHost>) {
    for line in output_lines(text) {
        let line = line.split_once('#').map_or(line, |(content, _)| content);
        if contains_unsafe_control(line, true) {
            continue;
        }
        let Some((keyword, patterns)) = split_ssh_directive(line) else {
            continue;
        };
        if !keyword.eq_ignore_ascii_case("host") {
            continue;
        }
        for host in patterns
            .split_ascii_whitespace()
            .filter_map(concrete_host_pattern)
        {
            merge_ssh_host(hosts, host, false, true, None);
        }
    }
}

fn split_ssh_directive(line: &str) -> Option<(&str, &str)> {
    let line = line.trim();
    let separator =
        line.find(|character: char| character.is_ascii_whitespace() || character == '=')?;
    let keyword = &line[..separator];
    let arguments = line[separator..].trim_start();
    let arguments = arguments
        .strip_prefix('=')
        .unwrap_or(arguments)
        .trim_start();
    (!keyword.is_empty() && !arguments.is_empty()).then_some((keyword, arguments))
}

fn concrete_known_host(pattern: &str) -> Option<(&str, Option<u16>)> {
    if excluded_ssh_pattern(pattern) {
        return None;
    }
    let Some(bracketed) = pattern.strip_prefix('[') else {
        return valid_host(pattern).then_some((pattern, None));
    };
    let (host, port) = bracketed.split_once("]:")?;
    let port = port.parse::<u16>().ok().filter(|port| *port != 0)?;
    valid_host(host).then_some((host, Some(port)))
}

fn concrete_host_pattern(pattern: &str) -> Option<&str> {
    (!excluded_ssh_pattern(pattern) && valid_host(pattern)).then_some(pattern)
}

fn excluded_ssh_pattern(pattern: &str) -> bool {
    pattern.is_empty()
        || pattern.starts_with(['!', '|'])
        || pattern.contains(['*', '?'])
        || (pattern.contains('[') && !pattern.starts_with('['))
}

fn valid_host(host: &str) -> bool {
    valid_value(host) && !host.contains([',', '[', ']'])
}

fn merge_ssh_host(
    hosts: &mut BTreeMap<String, ParsedSshHost>,
    host: &str,
    known_hosts: bool,
    config: bool,
    port: Option<u16>,
) {
    if !valid_item_fields(host, LONGEST_SSH_DESCRIPTION) {
        return;
    }
    let key = host.to_lowercase();
    if let Some(existing) = hosts.get_mut(&key) {
        existing.known_hosts |= known_hosts;
        existing.config |= config;
        existing.port = existing.port.or(port);
    } else if hosts.len() < MAX_DYNAMIC_ITEMS {
        hosts.insert(
            key,
            ParsedSshHost {
                value: host.to_owned(),
                known_hosts,
                config,
                port,
            },
        );
    }
}

#[derive(Clone, Debug)]
struct ScoredDirectory {
    path: String,
    score: f64,
    score_text: String,
}

impl ScoredDirectory {
    fn into_item(self) -> DynamicItem {
        let description = format!("zoxide score {}", self.score_text);
        DynamicItem {
            value: self.path,
            description,
            kind: DynamicItemKind::ZoxideDirectory,
            metadata: DynamicMetadata::Zoxide {
                score: self.score_text,
            },
        }
    }
}

fn split_score_path(line: &str) -> Option<(&str, &str)> {
    let line = line.trim();
    let separator = line.find(char::is_whitespace)?;
    let score = &line[..separator];
    let path = line[separator..].trim();
    (!score.is_empty() && !path.is_empty()).then_some((score, path))
}

fn split_optional_description(line: &str) -> Option<(&str, &str)> {
    let (value, description) = line.split_once('\t').map_or((line, ""), |parts| parts);
    if description.contains('\t') || value.is_empty() {
        return None;
    }
    Some((value, description))
}

fn decode_output(output: &[u8]) -> Result<&str, DynamicParseError> {
    if output.len() > MAX_DYNAMIC_OUTPUT_BYTES {
        return Err(DynamicParseError::OutputTooLarge {
            size: output.len(),
            limit: MAX_DYNAMIC_OUTPUT_BYTES,
        });
    }
    decode_utf8(output)
}

fn decode_utf8(output: &[u8]) -> Result<&str, DynamicParseError> {
    std::str::from_utf8(output).map_err(|_| DynamicParseError::InvalidUtf8)
}

fn output_lines(text: &str) -> impl Iterator<Item = &str> {
    text.split('\n')
        .map(|line| line.strip_suffix('\r').unwrap_or(line))
}

fn plain_item(value: &str, description: &str, kind: DynamicItemKind) -> DynamicItem {
    DynamicItem {
        value: value.to_owned(),
        description: description.to_owned(),
        kind,
        metadata: DynamicMetadata::None,
    }
}

fn insert_bounded<K: Ord>(map: &mut BTreeMap<K, DynamicItem>, key: K, item: DynamicItem) {
    if map.contains_key(&key) || map.len() < MAX_DYNAMIC_ITEMS {
        map.entry(key).or_insert(item);
    }
}

fn valid_item_fields(value: &str, description: &str) -> bool {
    valid_value(value)
        && !description.chars().any(char::is_control)
        && value.len().saturating_add(description.len()) <= MAX_DYNAMIC_ITEM_BYTES
}

fn valid_value(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_DYNAMIC_ITEM_BYTES
        && !value.chars().any(char::is_control)
}

fn contains_unsafe_control(value: &str, allow_tab: bool) -> bool {
    value
        .chars()
        .any(|character| character.is_control() && !(allow_tab && character == '\t'))
}

fn valid_git_ref(reference: &str) -> bool {
    valid_value(reference)
        && reference != "@"
        && !reference.starts_with(['/', '.'])
        && !reference.ends_with(['/', '.'])
        && !reference.contains("..")
        && !reference.contains("@{")
        && !reference.contains("//")
        && !reference
            .chars()
            .any(|character| matches!(character, ' ' | '~' | '^' | ':' | '?' | '*' | '[' | '\\'))
        && !reference.split('/').any(|component| {
            component.starts_with('.') || component.strip_suffix(".lock").is_some()
        })
}

fn valid_git_remote(remote: &str) -> bool {
    valid_git_ref(remote)
}

fn valid_stash_ref(reference: &str) -> bool {
    reference
        .strip_prefix("stash@{")
        .and_then(|index| index.strip_suffix('}'))
        .is_some_and(|index| !index.is_empty() && index.bytes().all(|byte| byte.is_ascii_digit()))
}

fn valid_object_id(object_id: &str) -> bool {
    (4..=64).contains(&object_id.len()) && object_id.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn valid_make_target(target: &str) -> bool {
    valid_value(target)
        && !matches!(
            target,
            ".DEFAULT"
                | ".DELETE_ON_ERROR"
                | ".EXPORT_ALL_VARIABLES"
                | ".IGNORE"
                | ".INTERMEDIATE"
                | ".LOW_RESOLUTION_TIME"
                | ".NOTINTERMEDIATE"
                | ".NOTPARALLEL"
                | ".ONESHELL"
                | ".PHONY"
                | ".POSIX"
                | ".PRECIOUS"
                | ".SECONDARY"
                | ".SECONDEXPANSION"
                | ".SILENT"
                | ".SUFFIXES"
                | ".WAIT"
        )
        && !target.chars().any(|character| {
            matches!(
                character,
                ':' | '=' | '%' | '$' | '#' | '*' | '?' | '[' | '\\'
            )
        })
}

fn valid_just_recipe(recipe: &str) -> bool {
    !recipe.is_empty()
        && recipe.len() <= MAX_DYNAMIC_ITEM_BYTES
        && recipe
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'+'))
}

fn valid_environment_name(name: &str) -> bool {
    let mut bytes = name.bytes();
    bytes
        .next()
        .is_some_and(|byte| byte.is_ascii_alphabetic() || byte == b'_')
        && name.len() <= MAX_DYNAMIC_ITEM_BYTES
        && bytes.all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
}

#[cfg(test)]
mod tests {
    use std::fmt::Write as _;

    use super::*;

    fn values(items: &[DynamicItem]) -> Vec<String> {
        items.iter().map(|item| item.value.clone()).collect()
    }

    #[test]
    fn git_branch_filters_are_explicit_and_local_refs_win() {
        let output = b"refs/remotes/origin/main\n\
            refs/remotes/upstream/feature/study\n\
            refs/remotes/foo/bar/main\n\
            refs/heads/main\n\
            * refs/heads/main\n\
            refs/remotes/origin/feature/study\n\
            refs/heads/feature/study\n\
            refs/remotes/origin/HEAD -> refs/remotes/origin/main\n";
        let cases = BTreeMap::from([
            (
                "default filtering",
                (
                    GitBranchOptions {
                        active_branch: Some("main"),
                        remote_names: &["origin", "upstream", "foo", "foo/bar"],
                        ..GitBranchOptions::default()
                    },
                    vec!["feature/study"],
                ),
            ),
            (
                "all refs",
                (
                    GitBranchOptions {
                        active_branch: Some("refs/heads/main"),
                        filter_active_branch: false,
                        deduplicate_branches: false,
                        remote_names: &["origin", "upstream", "foo", "foo/bar"],
                    },
                    vec![
                        "feature/study",
                        "foo/bar/main",
                        "main",
                        "origin/feature/study",
                        "origin/main",
                        "upstream/feature/study",
                    ],
                ),
            ),
        ]);

        for (name, (options, want)) in cases {
            let items = parse_git_branches(output, options).unwrap();
            assert_eq!(values(&items), want, "{name}");
        }
    }

    #[test]
    fn git_branch_metadata_marks_an_unfiltered_active_branch() {
        let items = parse_git_branches(
            b"main\n",
            GitBranchOptions {
                active_branch: Some("main"),
                filter_active_branch: false,
                deduplicate_branches: true,
                remote_names: &[],
            },
        )
        .unwrap();

        assert_eq!(items[0].description, "active Git branch");
        assert_eq!(
            items[0].metadata,
            DynamicMetadata::GitBranch {
                scope: GitBranchScope::Local,
                active: true,
            }
        );
    }

    #[test]
    fn parses_git_names_and_recent_described_rows() {
        let cases = BTreeMap::from([
            (
                "remotes",
                values(&parse_git_remotes(b"upstream\nfoo/bar\norigin\norigin\n").unwrap()),
            ),
            (
                "tags",
                values(&parse_git_tags(b"v2.0.0\nv1.0.0\nbad tag\n").unwrap()),
            ),
            (
                "stashes",
                values(
                    &parse_git_stashes(
                        b"stash@{0}\tOn main: Troy and Abed\nstash@{1}\tDean's list\n",
                    )
                    .unwrap(),
                ),
            ),
            (
                "commits",
                values(
                    &parse_git_commits(
                        b"a11ce55\tStudy group\nb0bca7\tSave Greendale\na11ce55\tolder\n",
                    )
                    .unwrap(),
                ),
            ),
        ]);

        assert_eq!(cases["remotes"], ["foo/bar", "origin", "upstream"]);
        assert_eq!(cases["tags"], ["v1.0.0", "v2.0.0"]);
        assert_eq!(cases["stashes"], ["stash@{0}", "stash@{1}"]);
        assert_eq!(cases["commits"], ["a11ce55", "b0bca7"]);
    }

    #[test]
    fn git_rows_reject_invalid_refs_and_describe_empty_subjects() {
        assert_eq!(
            values(&parse_git_tags(b"@\nfeature/.hidden\nvalid\n").unwrap()),
            ["valid"]
        );

        let stashes = parse_git_stashes(b"stash@{0}\t\n").unwrap();
        let commits = parse_git_commits(b"a11ce55\t\n").unwrap();
        assert_eq!(stashes[0].description, "Git stash");
        assert_eq!(commits[0].description, "Git commit");
    }

    #[test]
    fn parses_make_database_and_just_summary_without_interpretation() {
        let make = b"# Make data base\n\
            define MULTILINE\n\
            # Files\n\
            fake: variable row\n\
            endef\n\
            # Files\n\
            .PHONY: clean test\n\
            clean: ; @echo clean\n\
            study test &: prerequisite\n\
            .deploy:\n\
            %.o: %.c\n\
            escaped\\ target:\n\
            VARIABLE := value\n\
            $(EXPANDED):\n\
            # Not a target:\n\
            source.c:\n\
            clean: duplicate\n\
            # files hash-table stats:\n\
            after: ignored\n";
        let just = b"study-group save-greendale troy_and_abed study-group\n";

        assert_eq!(
            values(&parse_make_targets(make).unwrap()),
            [".deploy", "clean", "study", "test"]
        );
        assert_eq!(
            values(&parse_just_recipes(just).unwrap()),
            ["save-greendale", "study-group", "troy_and_abed"]
        );
        assert!(
            parse_make_targets(b"# Files\nunfinished:\n")
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn git_remote_disambiguation_has_a_bounded_input_snapshot() {
        let mut names = (0..MAX_DYNAMIC_ITEMS)
            .map(|index| format!("remote-{index}"))
            .collect::<Vec<_>>();
        names.push("foo/bar".to_owned());
        let names = names.iter().map(String::as_str).collect::<Vec<_>>();
        let items = parse_git_branches(
            b"refs/remotes/foo/bar/main\n",
            GitBranchOptions {
                remote_names: &names,
                ..GitBranchOptions::default()
            },
        )
        .unwrap();

        assert_eq!(
            items[0].metadata,
            DynamicMetadata::GitBranch {
                scope: GitBranchScope::Remote {
                    remote: "foo".to_owned(),
                },
                active: false,
            }
        );
    }

    #[test]
    fn merges_only_concrete_ssh_hosts_and_preserves_sources() {
        let known_hosts = b"greendale.edu,10.0.0.1\tssh-ed25519\tkey\n\
            |1|hash|value ssh-ed25519 key\n\
            *.example.com ssh-rsa key\n\
            [troy.greendale.edu]:2222 ssh-ed25519 key\n\
            @cert-authority abed.greendale.edu ssh-ed25519 key\n";
        let config = b"Host\tgreendale.edu annies-move *.invalid !chang\n\
            Host=dean-office\n\
            HostName ignored.example\n\
            host Abed.Greendale.edu\n";
        let items = parse_ssh_hosts(known_hosts, config).unwrap();

        assert_eq!(
            values(&items),
            [
                "10.0.0.1",
                "abed.greendale.edu",
                "annies-move",
                "dean-office",
                "greendale.edu",
                "troy.greendale.edu"
            ]
        );
        assert_eq!(
            items[1].metadata,
            DynamicMetadata::SshHost {
                known_hosts: true,
                config: true,
                port: None,
            }
        );
        assert_eq!(
            items[5].metadata,
            DynamicMetadata::SshHost {
                known_hosts: true,
                config: false,
                port: Some(2222),
            }
        );
    }

    #[test]
    fn parses_ranked_directories_processes_and_environment_names() {
        let directories = parse_zoxide_directories(
            b"12.5\t/tmp/Greendale Community College\n3 /tmp/Study Room\n14 /tmp/Study Room\n",
        )
        .unwrap();
        let processes =
            parse_processes(b" 42 dean-daemon\n7 study group\n0 init\nPID COMMAND\n").unwrap();
        let environment =
            environment_variable_items(["DEAN", "PATH", "BAD-NAME", "_EMPTY", "PATH"]);

        assert_eq!(
            values(&directories),
            ["/tmp/Study Room", "/tmp/Greendale Community College"]
        );
        assert_eq!(
            directories[0].metadata,
            DynamicMetadata::Zoxide { score: "14".into() }
        );
        assert_eq!(values(&processes), ["7", "42"]);
        assert_eq!(processes[0].description, "study group");
        assert_eq!(values(&environment), ["DEAN", "PATH", "_EMPTY"]);
    }

    #[test]
    fn structured_environment_names_cannot_reparse_value_lines() {
        let environment = environment_variable_items(["TOKEN=foo", "PASSWORD", "VALID"]);

        assert_eq!(values(&environment), ["PASSWORD", "VALID"]);
    }

    #[test]
    fn newline_resources_are_typed_deduplicated_and_described() {
        let cases = BTreeMap::from([
            (
                "containers",
                (
                    DynamicResourceKind::DockerContainer,
                    b"study-group\tcontainer a11ce\ntroy-and-abed\nstudy-group\tduplicate\n"
                        .as_slice(),
                    vec!["study-group", "troy-and-abed"],
                ),
            ),
            (
                "images",
                (
                    DynamicResourceKind::DockerImage,
                    b"greendale/community:latest\n".as_slice(),
                    vec!["greendale/community:latest"],
                ),
            ),
        ]);

        for (name, (kind, input, want)) in cases {
            let items = parse_resource_lines(input, kind).unwrap();
            assert_eq!(values(&items), want, "{name}");
            assert!(
                items
                    .iter()
                    .all(|item| item.kind == DynamicItemKind::Resource(kind))
            );
        }
    }

    #[test]
    fn unsafe_rows_are_isolated_and_whole_capture_limits_are_enforced() {
        let long_value = "x".repeat(MAX_DYNAMIC_ITEM_BYTES + 1);
        let input = format!("troy\nansi\u{1b}[31m\n{long_value}\nabed\n");
        let resources =
            parse_resource_lines(input.as_bytes(), DynamicResourceKind::Package).unwrap();
        assert_eq!(values(&resources), ["abed", "troy"]);

        assert_eq!(parse_git_tags(&[0xff]), Err(DynamicParseError::InvalidUtf8));
        let oversized = vec![b'x'; MAX_DYNAMIC_OUTPUT_BYTES + 1];
        assert_eq!(
            parse_git_tags(&oversized),
            Err(DynamicParseError::OutputTooLarge {
                size: MAX_DYNAMIC_OUTPUT_BYTES + 1,
                limit: MAX_DYNAMIC_OUTPUT_BYTES,
            })
        );
    }

    #[test]
    fn every_parser_stops_at_the_unique_item_limit() {
        let mut input = String::new();
        for index in 0..MAX_DYNAMIC_ITEMS + 25 {
            writeln!(input, "resource-{index}").unwrap();
        }
        let items = parse_resource_lines(input.as_bytes(), DynamicResourceKind::Service).unwrap();

        assert_eq!(items.len(), MAX_DYNAMIC_ITEMS);
    }
}
