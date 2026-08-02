#!/usr/bin/env bash
# Verified installer for argmax: downloads the latest release for this
# OS/architecture, checks its sha256 against the published checksums, and
# installs the binary to a user or system bin directory.
#
# Usage: curl -fsSL https://raw.githubusercontent.com/rselbach/argmax/main/scripts/install.sh | bash

set -euo pipefail

REPO="rselbach/argmax"
API="https://api.github.com/repos/${REPO}/releases/latest"

err() {
  echo "install.sh: $*" >&2
}

detect_platform() {
  local os arch
  case "$(uname -s)" in
    Linux) os="linux" ;;
    Darwin) os="darwin" ;;
    *)
      err "unsupported operating system: $(uname -s)"
      return 1
      ;;
  esac
  case "$(uname -m)" in
    x86_64 | amd64) arch="amd64" ;;
    arm64 | aarch64) arch="arm64" ;;
    *)
      err "unsupported architecture: $(uname -m)"
      return 1
      ;;
  esac
  echo "${os}_${arch}"
}

fetch() {
  local url="$1"
  local dest="$2"
  if command -v curl >/dev/null 2>&1; then
    curl -fsSL "${url}" -o "${dest}"
  elif command -v wget >/dev/null 2>&1; then
    wget -qO "${dest}" "${url}"
  else
    err "neither curl nor wget is available"
    return 1
  fi
}

sha256_check() {
  local sums="$1"
  local asset="$2"
  if command -v sha256sum >/dev/null 2>&1; then
    (grep " ${asset}\$" "${sums}" | sha256sum -c - >/dev/null)
  elif command -v shasum >/dev/null 2>&1; then
    (grep " ${asset}\$" "${sums}" | shasum -a 256 -c - >/dev/null)
  else
    err "no sha256 tool available for verification"
    return 1
  fi
}

pick_bindir() {
  local dir="${HOME}/.local/bin"
  if [[ -w /usr/local/bin ]]; then
    dir="/usr/local/bin"
  fi
  mkdir -p "${dir}"
  echo "${dir}"
}

main() {
  local platform tag asset workdir bindir
  platform="$(detect_platform)"
  asset="argmax_${platform}.tar.gz"

  tag="$(fetch "${API}" /dev/stdout | grep -m1 '"tag_name"' | cut -d'"' -f4)"
  if [[ -z "${tag}" ]]; then
    err "could not determine the latest release"
    exit 1
  fi
  echo "installing argmax ${tag} (${platform})"

  workdir="$(mktemp -d)"
  trap 'rm -rf "${workdir}"' EXIT

  local base="https://github.com/${REPO}/releases/download/${tag}"
  fetch "${base}/${asset}" "${workdir}/${asset}"
  fetch "${base}/checksums.txt" "${workdir}/checksums.txt"

  (cd "${workdir}" && sha256_check checksums.txt "${asset}")
  echo "checksum verified"

  tar -xzf "${workdir}/${asset}" -C "${workdir}" argmax
  bindir="$(pick_bindir)"
  install -m 0755 "${workdir}/argmax" "${bindir}/argmax"
  echo "installed ${bindir}/argmax"

  case ":${PATH}:" in
    *":${bindir}:"*) ;;
    *) echo "note: add ${bindir} to your PATH" ;;
  esac
  echo "next: run 'argmax setup' to install shell integration"
}

main "$@"
