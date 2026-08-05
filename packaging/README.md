# Packaging and distribution

Published channels ship the same `argmax` binary built from `cmd/argmax` with
`-X main.version=<version>`; see `.goreleaser.yaml` for the release
pipeline. Archive names (`argmax_<os>_<arch>.tar.gz`, `checksums.txt`)
are load-bearing: `argmax update` downloads and verifies them.

The checked-in AUR and Nix files are release-maintainer templates, not
ready-to-use installation channels. GoReleaser creates a deterministic
`argmax-<version>-source.tar.gz`, includes it in `checksums.txt`, and the tag
workflow renders release-specific AUR metadata as a workflow artifact. Before
publishing that metadata to AUR, review it and commit it to the AUR package
repository. Nix still requires a real `vendorHash` and reviewed `flake.lock`.
The placeholders deliberately fail closed; never replace them with `SKIP` or
another verification bypass.

| Channel | Source | Status |
| --- | --- | --- |
| GitHub releases (tar.gz + checksums) | goreleaser on tag push | automated |
| Homebrew tap | `brews` section, pushes to `rselbach/homebrew-tap` | automated (needs `HOMEBREW_TAP_GITHUB_TOKEN`) |
| Debian/Ubuntu `.deb`, Fedora/RHEL `.rpm` | goreleaser `nfpms` | automated |
| Install script | `scripts/install.sh` (checksum-verified) | in repo |
| AUR | `packaging/aur/PKGBUILD` | release template; CI renders checksummed metadata for manual publishing |
| Nix flake | `flake.nix` | release template; lock file and `vendorHash` required before use |
| `go install github.com/rselbach/argmax/cmd/argmax@latest` | Go toolchain | works from any tagged release |
| aqua registry | submit `aquaproj/aqua-registry` entry referencing the GitHub release assets | external PR per the registry contribution guide |
| asdf plugin | create `asdf-argmax` repo wrapping the release download URL pattern | external repo |

Nightly releases are tags like `v1.4.0-nightly.20260801`; goreleaser marks
them prereleases, which the stable update channel ignores and the nightly
channel selects.

Package uninstall notes: the deb/rpm `preremove` script directs users to
`argmax uninstall`, which removes per-user shell integration the package
manager cannot see.
