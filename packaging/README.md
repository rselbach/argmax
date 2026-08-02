# Packaging and distribution

All channels ship the same `argmax` binary built from `cmd/argmax` with
`-X main.version=<version>`; see `.goreleaser.yaml` for the release
pipeline. Archive names (`argmax_<os>_<arch>.tar.gz`, `checksums.txt`)
are load-bearing: `argmax update` downloads and verifies them.

| Channel | Source | Status |
| --- | --- | --- |
| GitHub releases (tar.gz + checksums) | goreleaser on tag push | automated |
| Homebrew tap | `brews` section, pushes to `rselbach/homebrew-tap` | automated (needs `HOMEBREW_TAP_GITHUB_TOKEN`) |
| Debian/Ubuntu `.deb`, Fedora/RHEL `.rpm` | goreleaser `nfpms` | automated |
| Install script | `scripts/install.sh` (checksum-verified) | in repo |
| AUR | `packaging/aur/PKGBUILD` | manual publish per release |
| Nix flake | `flake.nix` (set `vendorHash` on first build) | in repo |
| `go install github.com/rselbach/argmax/cmd/argmax@latest` | Go toolchain | works from any tagged release |
| aqua registry | submit `aquaproj/aqua-registry` entry referencing the GitHub release assets | external PR per the registry contribution guide |
| asdf plugin | create `asdf-argmax` repo wrapping the release download URL pattern | external repo |

Nightly releases are tags like `v1.4.0-nightly.20260801`; goreleaser marks
them prereleases, which the stable update channel ignores and the nightly
channel selects.

Package uninstall notes: the deb/rpm `preremove` script directs users to
`argmax uninstall`, which removes per-user shell integration the package
manager cannot see.
