//! Deterministic workspace-context scoring for canonical command skeletons.

use crate::providers::{WorkspaceContext, WorkspaceKind};

/// Lowest normalized workspace-context score.
pub const MIN_CONTEXT_SCORE: f64 = 0.0;
/// Highest normalized workspace-context score.
pub const MAX_CONTEXT_SCORE: f64 = 1.0;
/// Score for a command appropriate to a detected workspace ecosystem.
pub const ECOSYSTEM_MATCH_SCORE: f64 = MAX_CONTEXT_SCORE;
/// Neutral midpoint for unrelated, unknown, or context-free commands.
pub const NEUTRAL_CONTEXT_SCORE: f64 = 0.5;
/// Penalty for initializing Git inside an existing Git repository.
pub const EXISTING_GIT_INIT_SCORE: f64 = MIN_CONTEXT_SCORE;

/// Inspectable explanation for one workspace-context score.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ContextReason {
    /// The command belongs to an ecosystem detected in the current workspace.
    EcosystemDetected(WorkspaceKind),
    /// The command is known, but its ecosystem was not detected here.
    EcosystemNotDetected(WorkspaceKind),
    /// `git init` is inappropriate because Git metadata already exists.
    GitAlreadyInitialized,
    /// The canonical command root has no ecosystem mapping.
    UnknownCommand,
    /// The supplied skeleton was empty or not canonical single-space-separated text.
    InvalidSkeleton,
}

/// Normalized workspace-context signal and its deterministic explanation.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ContextScore {
    /// Score in the inclusive range [`MIN_CONTEXT_SCORE`] through
    /// [`MAX_CONTEXT_SCORE`].
    pub normalized_score: f64,
    /// Rule that produced the score.
    pub reason: ContextReason,
}

impl ContextScore {
    const fn new(normalized_score: f64, reason: ContextReason) -> Self {
        Self {
            normalized_score,
            reason,
        }
    }
}

/// Scores a canonical command skeleton against detected workspace ecosystems.
///
/// Matching is based on the canonical executable at the start of `skeleton`.
/// Subcommands therefore inherit their executable's ecosystem. Known commands
/// without a matching marker and unknown commands remain neutral.
#[must_use]
pub fn score_workspace_context(context: &WorkspaceContext, skeleton: &str) -> ContextScore {
    let Some((root, subcommand)) = skeleton_root_and_subcommand(skeleton) else {
        return ContextScore::new(NEUTRAL_CONTEXT_SCORE, ContextReason::InvalidSkeleton);
    };

    if root == "git" && subcommand == Some("init") && context.contains(WorkspaceKind::Git) {
        return ContextScore::new(
            EXISTING_GIT_INIT_SCORE,
            ContextReason::GitAlreadyInitialized,
        );
    }

    let Some(kind) = command_workspace_kind(root) else {
        return ContextScore::new(NEUTRAL_CONTEXT_SCORE, ContextReason::UnknownCommand);
    };

    if context.contains(kind) {
        ContextScore::new(
            ECOSYSTEM_MATCH_SCORE,
            ContextReason::EcosystemDetected(kind),
        )
    } else {
        ContextScore::new(
            NEUTRAL_CONTEXT_SCORE,
            ContextReason::EcosystemNotDetected(kind),
        )
    }
}

/// Returns the ecosystem associated with a canonical executable basename.
#[must_use]
pub fn command_workspace_kind(command: &str) -> Option<WorkspaceKind> {
    ECOSYSTEM_COMMANDS
        .iter()
        .find(|mapping| mapping.commands.contains(&command))
        .map(|mapping| mapping.kind)
}

#[derive(Clone, Copy)]
struct EcosystemCommands {
    kind: WorkspaceKind,
    commands: &'static [&'static str],
}

const ECOSYSTEM_COMMANDS: [EcosystemCommands; 9] = [
    EcosystemCommands {
        kind: WorkspaceKind::Git,
        commands: &[
            "git", "gh", "git-flow", "git-lfs", "gitk", "hub", "lazygit", "tig",
        ],
    },
    EcosystemCommands {
        kind: WorkspaceKind::Node,
        commands: &[
            "babel",
            "bun",
            "bunx",
            "corepack",
            "deno",
            "eslint",
            "node",
            "npm",
            "npx",
            "pnpm",
            "pnpx",
            "prettier",
            "rollup",
            "ts-node",
            "tsc",
            "tsserver",
            "tsx",
            "vite",
            "webpack",
            "webpack-cli",
            "yarn",
            "yarnpkg",
        ],
    },
    EcosystemCommands {
        kind: WorkspaceKind::Go,
        commands: &["go", "gofmt", "goimports", "golangci-lint", "mage"],
    },
    EcosystemCommands {
        kind: WorkspaceKind::Rust,
        commands: &[
            "cargo",
            "cargo-clippy",
            "cargo-miri",
            "clippy-driver",
            "mdbook",
            "rust-analyzer",
            "rust-gdb",
            "rust-lldb",
            "rustc",
            "rustdoc",
            "rustfmt",
            "rustup",
        ],
    },
    EcosystemCommands {
        kind: WorkspaceKind::Python,
        commands: &[
            "black", "flake8", "ipython", "isort", "jupyter", "mypy", "nox", "pdm", "pip", "pip3",
            "pipenv", "pipx", "poetry", "pytest", "python", "python3", "ruff", "tox", "uv",
        ],
    },
    EcosystemCommands {
        kind: WorkspaceKind::Docker,
        commands: &[
            "buildah",
            "dive",
            "docker",
            "docker-compose",
            "hadolint",
            "lazydocker",
            "nerdctl",
            "podman",
            "podman-compose",
            "skopeo",
        ],
    },
    EcosystemCommands {
        kind: WorkspaceKind::Make,
        commands: &["bmake", "gmake", "make"],
    },
    EcosystemCommands {
        kind: WorkspaceKind::Just,
        commands: &["just"],
    },
    EcosystemCommands {
        kind: WorkspaceKind::Kubernetes,
        commands: &[
            "helm",
            "k3d",
            "k9s",
            "kind",
            "kubectl",
            "kubectx",
            "kubens",
            "kustomize",
            "minikube",
            "skaffold",
            "stern",
        ],
    },
];

fn skeleton_root_and_subcommand(skeleton: &str) -> Option<(&str, Option<&str>)> {
    if skeleton.is_empty()
        || skeleton.split(' ').any(|token| {
            token.is_empty()
                || token
                    .chars()
                    .any(|character| character.is_whitespace() || character.is_control())
        })
    {
        return None;
    }

    let mut tokens = skeleton.split(' ');
    let root = tokens.next()?;
    Some((root, tokens.next()))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    use crate::providers::WorkspaceSignature;

    use super::*;

    const GREENDALE: &str = "/home/troy/Greendale";

    fn community_context(kinds: &[WorkspaceKind]) -> WorkspaceContext {
        let root = PathBuf::from(GREENDALE);
        WorkspaceContext {
            cwd: root.join("library/study-room-f"),
            signatures: kinds
                .iter()
                .map(|kind| WorkspaceSignature {
                    kind: *kind,
                    root: root.clone(),
                    marker: root.join(format!("{kind:?}-marker")),
                })
                .collect(),
        }
    }

    fn assert_score(actual: f64, want: f64) {
        assert!(
            (actual - want).abs() <= f64::EPSILON,
            "got {actual}, want {want}"
        );
    }

    #[test]
    fn every_workspace_ecosystem_boosts_appropriate_commands() {
        let cases = BTreeMap::from([
            ("Annie organizes Python", (WorkspaceKind::Python, "pytest")),
            ("Britta ships Node", (WorkspaceKind::Node, "npm test")),
            ("Chang makes", (WorkspaceKind::Make, "make")),
            (
                "Dean deploys Kubernetes",
                (WorkspaceKind::Kubernetes, "kubectl apply"),
            ),
            ("Jeff uses Git", (WorkspaceKind::Git, "git status")),
            (
                "Pierce runs Docker",
                (WorkspaceKind::Docker, "docker compose"),
            ),
            ("Shirley cooks with Just", (WorkspaceKind::Just, "just")),
            ("Troy learns Rust", (WorkspaceKind::Rust, "cargo test")),
            ("Abed directs Go", (WorkspaceKind::Go, "go test")),
        ]);

        for (name, (kind, skeleton)) in cases {
            let scored = score_workspace_context(&community_context(&[kind]), skeleton);
            assert_score(scored.normalized_score, ECOSYSTEM_MATCH_SCORE);
            assert_eq!(
                scored.reason,
                ContextReason::EcosystemDetected(kind),
                "{name}"
            );
        }
    }

    #[test]
    fn git_init_is_penalized_only_inside_an_existing_repository() {
        let cases = BTreeMap::from([
            (
                "existing Greendale repository",
                (
                    community_context(&[WorkspaceKind::Git]),
                    EXISTING_GIT_INIT_SCORE,
                    ContextReason::GitAlreadyInitialized,
                ),
            ),
            (
                "new Greendale project",
                (
                    community_context(&[]),
                    NEUTRAL_CONTEXT_SCORE,
                    ContextReason::EcosystemNotDetected(WorkspaceKind::Git),
                ),
            ),
        ]);

        for (name, (context, want_score, want_reason)) in cases {
            let scored = score_workspace_context(&context, "git init");
            assert_score(scored.normalized_score, want_score);
            assert_eq!(scored.reason, want_reason, "{name}");
        }
    }

    #[test]
    fn unrelated_known_and_unknown_commands_remain_neutral() {
        let rust_context = community_context(&[WorkspaceKind::Rust]);
        let cases = BTreeMap::from([
            (
                "known but unrelated Node command",
                (
                    "npm test",
                    ContextReason::EcosystemNotDetected(WorkspaceKind::Node),
                ),
            ),
            (
                "known but unrelated Git command",
                (
                    "git status",
                    ContextReason::EcosystemNotDetected(WorkspaceKind::Git),
                ),
            ),
            (
                "unknown paintball command",
                ("troy", ContextReason::UnknownCommand),
            ),
        ]);

        for (name, (skeleton, want_reason)) in cases {
            let scored = score_workspace_context(&rust_context, skeleton);
            assert_score(scored.normalized_score, NEUTRAL_CONTEXT_SCORE);
            assert_eq!(scored.reason, want_reason, "{name}");
        }
    }

    #[test]
    fn malformed_skeletons_are_inspectably_neutral() {
        let cases = BTreeMap::from([
            ("empty", ""),
            ("leading space", " git status"),
            ("repeated space", "git  status"),
            ("tab separator", "git\tstatus"),
            ("trailing space", "git status "),
        ]);
        let context = community_context(&[WorkspaceKind::Git]);

        for (name, skeleton) in cases {
            let scored = score_workspace_context(&context, skeleton);
            assert_score(scored.normalized_score, NEUTRAL_CONTEXT_SCORE);
            assert_eq!(scored.reason, ContextReason::InvalidSkeleton, "{name}");
        }
    }

    #[test]
    fn scoring_is_independent_of_signature_order() {
        let first = community_context(&[WorkspaceKind::Git, WorkspaceKind::Node]);
        let second = community_context(&[WorkspaceKind::Node, WorkspaceKind::Git]);
        let cases = BTreeMap::from([
            ("Git subcommand", "git fetch"),
            ("Node subcommand", "pnpm test"),
            ("unknown command", "greendale"),
        ]);

        for (name, skeleton) in cases {
            assert_eq!(
                score_workspace_context(&first, skeleton),
                score_workspace_context(&second, skeleton),
                "{name}"
            );
        }
    }

    #[test]
    fn command_mapping_is_unique_and_data_driven() {
        let mut owners = BTreeMap::new();
        for mapping in ECOSYSTEM_COMMANDS {
            for command in mapping.commands {
                assert!(
                    owners.insert(*command, mapping.kind).is_none(),
                    "duplicate mapping for {command}"
                );
                assert_eq!(command_workspace_kind(command), Some(mapping.kind));
            }
        }
    }
}
