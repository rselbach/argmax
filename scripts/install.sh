#!/bin/sh
# Install one verified argmax release binary without modifying user state.

set -eu

PROGRAM_NAME=argmax
DEFAULT_REPOSITORY=rselbach/argmax
DOWNLOAD_CONNECT_TIMEOUT_SECONDS=10
DOWNLOAD_TIMEOUT_SECONDS=120

install_temp_dir=
install_staged_path=
install_tmp_root=

fail() {
  printf '%s: %s\n' "${PROGRAM_NAME}" "$*" >&2
  exit 1
}

cleanup() {
  if [ -n "${install_staged_path}" ] && [ -f "${install_staged_path}" ]; then
    rm -f "${install_staged_path}"
  fi
  if [ -n "${install_temp_dir}" ]; then
    case "${install_temp_dir}" in
      "${install_tmp_root}"/argmax-install.*)
        rm -rf "${install_temp_dir}"
        ;;
    esac
  fi
}

command_exists() {
  command -v "$1" >/dev/null 2>&1
}

text_is_safe() {
  [ -n "$1" ] || return 1
  install_sanitized=$(
    printf '%s' "$1" | LC_ALL=C tr -d '\001-\037\177'
  ) || return 1
  [ "${install_sanitized}" = "$1" ]
}

is_semantic_version() {
  [ -n "$1" ] || return 1
  printf '%s\n' "$1" | LC_ALL=C awk '
    NR != 1 { exit 1 }
    $0 !~ /^[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z-]+(\.[0-9A-Za-z-]+)*)?(\+[0-9A-Za-z-]+(\.[0-9A-Za-z-]+)*)?$/ {
      exit 1
    }
    {
      semantic = $0
      sub(/\+.*/, "", semantic)
      core = semantic
      sub(/-.*/, "", core)
      count = split(core, numbers, ".")
      if (count != 3) { exit 1 }
      for (part = 1; part <= count; part++) {
        if (length(numbers[part]) > 1 && substr(numbers[part], 1, 1) == "0") {
          exit 1
        }
      }
      if (length(semantic) > length(core)) {
        prerelease = substr(semantic, length(core) + 2)
        count = split(prerelease, identifiers, ".")
        for (part = 1; part <= count; part++) {
          if (identifiers[part] ~ /^[0-9]+$/ \
              && length(identifiers[part]) > 1 \
              && substr(identifiers[part], 1, 1) == "0") {
            exit 1
          }
        }
      }
      valid = 1
    }
    END { if (!valid) { exit 1 } }
  '
}

version_matches_channel() {
  install_version_value=$1
  install_precedence=${install_version_value%%+*}
  case "${install_channel}:${install_precedence}" in
    stable:*-*) return 1 ;;
    stable:*) return 0 ;;
    nightly:*-*) return 0 ;;
    nightly:*) return 1 ;;
  esac
  return 1
}

detect_asset_suffix() {
  install_system=$(uname -s 2>/dev/null) || fail "could not detect the operating system"
  install_machine=$(uname -m 2>/dev/null) || fail "could not detect the CPU architecture"

  case "${install_system}" in
    Linux)
      install_os=linux
      ;;
    Darwin)
      install_os=macos
      ;;
    *)
      fail "unsupported operating system: ${install_system}"
      ;;
  esac

  case "${install_machine}" in
    x86_64 | amd64)
      install_arch=amd64
      ;;
    arm64 | aarch64)
      install_arch=arm64
      ;;
    *)
      fail "unsupported CPU architecture: ${install_machine}"
      ;;
  esac

  printf '%s-%s\n' "${install_os}" "${install_arch}"
}

release_base_url() {
  if [ -n "${ARGMAX_RELEASE_BASE_URL:-}" ]; then
    install_release_base=${ARGMAX_RELEASE_BASE_URL}
  else
    install_repository=${ARGMAX_REPOSITORY:-${DEFAULT_REPOSITORY}}
    case "${install_repository}" in
      *[!A-Za-z0-9._/-]* | /* | */ | *//* | */*/*)
        fail "ARGMAX_REPOSITORY must be one safe owner/repository pair"
        ;;
      */*) ;;
      *) fail "ARGMAX_REPOSITORY must be an owner/repository pair" ;;
    esac
    case "${install_requested_version}" in
      '')
        if [ "${install_channel}" = nightly ]; then
          install_release_base="https://github.com/${install_repository}/releases/download/nightly"
        elif [ "${install_channel}" = stable ]; then
          install_release_base="https://github.com/${install_repository}/releases/latest/download"
        fi
        ;;
      *)
        install_release_base="https://github.com/${install_repository}/releases/download/v${install_requested_version}"
        ;;
    esac
  fi

  text_is_safe "${install_release_base}" \
    || fail "release URL contains unsupported control text"
  case "${install_release_base}" in
    https://*) ;;
    *) fail "release downloads must use HTTPS" ;;
  esac
  printf '%s\n' "${install_release_base%/}"
}

download_file() {
  install_url=$1
  install_destination=$2
  install_download_name=${install_url##*/}
  if command_exists curl; then
    curl --fail --location --silent --show-error \
      --proto '=https' --tlsv1.2 \
      --connect-timeout "${DOWNLOAD_CONNECT_TIMEOUT_SECONDS}" \
      --max-time "${DOWNLOAD_TIMEOUT_SECONDS}" \
      --output "${install_destination}" "${install_url}" \
      || fail "download failed for ${install_download_name}"
    return
  fi
  if command_exists wget; then
    wget --quiet --https-only \
      --timeout="${DOWNLOAD_CONNECT_TIMEOUT_SECONDS}" --tries=2 \
      --output-document="${install_destination}" "${install_url}" \
      || fail "download failed for ${install_download_name}"
    return
  fi
  fail "curl or wget is required to download a release"
}

sha256_file() {
  install_path=$1
  if command_exists sha256sum; then
    sha256sum "${install_path}" | awk '{ print $1 }'
    return
  fi
  if command_exists shasum; then
    shasum -a 256 "${install_path}" | awk '{ print $1 }'
    return
  fi
  fail "sha256sum or shasum is required to verify the release"
}

expected_sha256() {
  install_checksum_path=$1
  install_asset_name=$2
  awk -v asset="${install_asset_name}" '
    {
      name = $2
      sub(/^\*/, "", name)
      if (name == asset && length($1) == 64 && $1 !~ /[^0-9a-f]/) {
        print $1
        exit
      }
    }
  ' "${install_checksum_path}"
}

directory_is_writable() {
  install_candidate=$1
  if [ -d "${install_candidate}" ]; then
    [ -w "${install_candidate}" ]
    return
  fi
  if [ -e "${install_candidate}" ] || [ -L "${install_candidate}" ]; then
    return 1
  fi
  install_parent=${install_candidate%/*}
  [ "${install_parent}" != "${install_candidate}" ] \
    && [ -d "${install_parent}" ] \
    && [ -w "${install_parent}" ]
}

path_metadata() {
  install_metadata_path=$1
  if stat -c '%u %a' "${install_metadata_path}" >/dev/null 2>&1; then
    install_metadata=$(stat -c '%u %a' "${install_metadata_path}")
  elif stat -f '%u %p' "${install_metadata_path}" >/dev/null 2>&1; then
    install_metadata=$(stat -f '%u %p' "${install_metadata_path}")
  else
    return 1
  fi
  install_path_owner=${install_metadata%% *}
  install_path_mode=${install_metadata#* }
  case "${install_path_owner}" in
    '' | *[!0-9]*) return 1 ;;
  esac
  case "${install_path_mode}" in
    '' | *[!0-7]*) return 1 ;;
  esac
  # BSD %p includes file-type bits; all permission checks use bit masks.
  [ "${#install_path_mode}" -le 6 ] || return 1
  install_path_mode_value=$((0${install_path_mode}))
}

path_has_no_acl() {
  install_acl_path=$1
  # The first long-list field is the portable ACL marker exposed by both GNU
  # and BSD ls; filenames are not parsed from this output.
  # shellcheck disable=SC2012
  install_permissions=$(LC_ALL=C ls -ld "${install_acl_path}" | awk '{ print $1 }') \
    || return 1
  [ -n "${install_permissions}" ] || return 1
  case "${install_permissions}" in
    *+*) return 1 ;;
  esac
  return 0
}

directory_is_private() {
  install_private_directory=$1
  path_metadata "${install_private_directory}" || return 1
  [ "${install_path_owner}" = "$(id -u)" ] || return 1
  [ $((install_path_mode_value & 0022)) -eq 0 ] || return 1
  path_has_no_acl "${install_private_directory}"
}

path_entry_is_protected() {
  install_entry_parent=$1
  install_entry_child=$2
  path_metadata "${install_entry_parent}" || return 1
  path_has_no_acl "${install_entry_parent}" || return 1
  install_parent_owner=${install_path_owner}
  install_parent_mode_value=${install_path_mode_value}
  [ $((install_parent_mode_value & 0022)) -ne 0 ] || return 0
  [ $((install_parent_mode_value & 01000)) -ne 0 ] || return 1

  path_metadata "${install_entry_child}" || return 1
  install_current_uid=$(id -u) || return 1
  [ "${install_parent_owner}" = "${install_current_uid}" ] \
    || [ "${install_path_owner}" = "${install_current_uid}" ]
}

path_chain_is_secure() {
  install_chain_path=$1
  case "${install_chain_path}" in
    /*) ;;
    *) return 1 ;;
  esac
  [ "${install_chain_path}" != / ] || return 0

  install_chain_current=/
  install_chain_remaining=${install_chain_path#/}
  while [ -n "${install_chain_remaining}" ]; do
    case "${install_chain_remaining}" in
      */*)
        install_chain_component=${install_chain_remaining%%/*}
        install_chain_remaining=${install_chain_remaining#*/}
        ;;
      *)
        install_chain_component=${install_chain_remaining}
        install_chain_remaining=
        ;;
    esac
    [ -n "${install_chain_component}" ] || return 1
    install_chain_parent=${install_chain_current}
    if [ "${install_chain_current}" = / ]; then
      install_chain_current=/${install_chain_component}
    else
      install_chain_current=${install_chain_current}/${install_chain_component}
    fi
    [ -d "${install_chain_current}" ] \
      && [ ! -L "${install_chain_current}" ] \
      || return 1
    path_entry_is_protected \
      "${install_chain_parent}" "${install_chain_current}" \
      || return 1
  done
}

directory_accepts_private_children() {
  install_parent_directory=$1
  path_metadata "${install_parent_directory}" || return 1
  path_has_no_acl "${install_parent_directory}" || return 1
  [ $((install_path_mode_value & 0022)) -ne 0 ] || return 0
  [ $((install_path_mode_value & 01000)) -ne 0 ] || return 1
  install_current_uid=$(id -u) || return 1
  [ "${install_path_owner}" = 0 ] \
    || [ "${install_path_owner}" = "${install_current_uid}" ]
}

shell_quote() {
  install_quote_value=$1
  printf "'"
  while :; do
    case "${install_quote_value}" in
      *"'"*)
        install_quote_prefix=${install_quote_value%%"'"*}
        printf "%s'\\\\''" "${install_quote_prefix}"
        install_quote_value=${install_quote_value#*"'"}
        ;;
      *)
        printf "%s'" "${install_quote_value}"
        break
        ;;
    esac
  done
}

select_install_directory() {
  install_requested=${ARGMAX_INSTALL_DIR:-/usr/local/bin}
  text_is_safe "${install_requested}" \
    || fail "ARGMAX_INSTALL_DIR contains unsupported control text"
  case "${install_requested}" in
    /*) ;;
    *) fail "ARGMAX_INSTALL_DIR must be an absolute path" ;;
  esac

  if directory_is_writable "${install_requested}"; then
    mkdir -p "${install_requested}" \
      || fail "could not create ${install_requested}"
    install_resolved=$(CDPATH='' cd -P "${install_requested}" && pwd) \
      || fail "could not resolve the requested installation directory"
    text_is_safe "${install_resolved}" \
      || fail "resolved installation directory contains unsupported control text"
    if directory_is_private "${install_resolved}" \
      && path_chain_is_secure "${install_resolved}"; then
      printf '%s\n' "${install_resolved}"
      return
    fi
    printf '%s\n' \
      "argmax: requested directory is not private; using a user-local directory" >&2
  elif [ -e "${install_requested}" ] || [ -L "${install_requested}" ]; then
    printf '%s\n' \
      "argmax: requested path is not a directory; using a user-local directory" >&2
  fi

  [ -n "${HOME:-}" ] || fail "HOME is required for a user-local installation"
  text_is_safe "${HOME}" || fail "HOME contains unsupported control text"
  case "${HOME}" in
    /*) ;;
    *) fail "HOME must be an absolute path" ;;
  esac
  install_fallback=${HOME}/.local/bin
  (umask 077 && mkdir -p "${install_fallback}") \
    || fail "could not create user executable directory ${install_fallback}"
  [ -w "${install_fallback}" ] \
    || fail "user executable directory is not writable: ${install_fallback}"
  install_resolved=$(CDPATH='' cd -P "${install_fallback}" && pwd) \
    || fail "could not resolve the user executable directory"
  text_is_safe "${install_resolved}" \
    || fail "resolved user executable directory contains unsupported control text"
  directory_is_private "${install_resolved}" \
    || fail "user executable directory is not private to the current user"
  path_chain_is_secure "${install_resolved}" \
    || fail "an ancestor of the user executable directory is not secure"
  printf '%s\n' "${install_resolved}"
}

validate_staged_binary() {
  install_binary=$1
  install_reported_version=$(
    "${install_binary}" version 2>/dev/null
  ) || fail "the downloaded artifact does not run on this host"
  is_semantic_version "${install_reported_version}" \
    || fail "the downloaded artifact reported an invalid semantic version"
  version_matches_channel "${install_reported_version}" \
    || fail "the artifact version does not match the selected release channel"

  if [ -n "${install_requested_version}" ]; then
    [ "${install_reported_version}" = "${install_requested_version}" ] \
      || fail "the artifact version does not match ARGMAX_VERSION"
  fi
  install_validated_version=${install_reported_version}
}

publish_binary() {
  install_source=$1
  install_directory=$2
  install_expected_hash=$3
  [ ! -d "${install_directory}/${PROGRAM_NAME}" ] \
    || fail "installation target is a directory"
  install_staged_path=$(mktemp "${install_directory}/.argmax-install.XXXXXX") \
    || fail "could not create an installation transaction"
  cp "${install_source}" "${install_staged_path}" \
    || fail "could not stage the verified executable"
  [ "$(sha256_file "${install_staged_path}")" = "${install_expected_hash}" ] \
    || fail "staged executable checksum changed before validation"
  chmod 0755 "${install_staged_path}" \
    || fail "could not make the staged executable runnable"
  validate_staged_binary "${install_staged_path}"
  mv -f "${install_staged_path}" "${install_directory}/${PROGRAM_NAME}" \
    || fail "could not publish the verified executable"
  install_staged_path=
  install_published_version=${install_validated_version}
}

main() {
  trap cleanup EXIT HUP INT TERM

  install_channel=${ARGMAX_CHANNEL:-stable}
  case "${install_channel}" in
    stable | nightly) ;;
    *) fail "ARGMAX_CHANNEL must be stable or nightly" ;;
  esac
  install_requested_version=${ARGMAX_VERSION:-}
  if [ -n "${install_requested_version}" ]; then
    install_requested_version=${install_requested_version#v}
    is_semantic_version "${install_requested_version}" \
      || fail "ARGMAX_VERSION must be a semantic version"
    version_matches_channel "${install_requested_version}" \
      || fail "ARGMAX_VERSION does not match ARGMAX_CHANNEL"
  fi
  install_requested_tmp_root=${TMPDIR:-/tmp}
  text_is_safe "${install_requested_tmp_root}" \
    || fail "TMPDIR contains unsupported control text"
  case "${install_requested_tmp_root}" in
    /*) ;;
    *) fail "TMPDIR must be an absolute path" ;;
  esac
  if [ ! -d "${install_requested_tmp_root}" ] \
    || [ ! -w "${install_requested_tmp_root}" ]; then
    fail "TMPDIR must be an existing writable directory"
  fi
  install_tmp_root=$(CDPATH='' cd -P "${install_requested_tmp_root}" && pwd) \
    || fail "could not resolve TMPDIR"
  text_is_safe "${install_tmp_root}" \
    || fail "resolved TMPDIR contains unsupported control text"
  path_chain_is_secure "${install_tmp_root}" \
    || fail "an ancestor of TMPDIR does not protect private temporary files"
  directory_accepts_private_children "${install_tmp_root}" \
    || fail "TMPDIR does not protect private temporary files"

  install_suffix=$(detect_asset_suffix)
  install_asset="${PROGRAM_NAME}-${install_suffix}"
  install_checksum_asset="${install_asset}.sha256"
  install_base=$(release_base_url)
  install_temp_dir=$(mktemp -d "${install_tmp_root}/argmax-install.XXXXXX") \
    || fail "could not create a private temporary directory"
  chmod 0700 "${install_temp_dir}" \
    || fail "could not secure the temporary directory"
  directory_is_private "${install_temp_dir}" \
    || fail "could not verify the private temporary directory"
  path_chain_is_secure "${install_temp_dir}" \
    || fail "could not verify temporary directory ancestors"
  install_download="${install_temp_dir}/${install_asset}"
  install_checksum="${install_temp_dir}/${install_checksum_asset}"

  download_file "${install_base}/${install_asset}" "${install_download}"
  download_file "${install_base}/${install_checksum_asset}" "${install_checksum}"

  install_expected=$(expected_sha256 "${install_checksum}" "${install_asset}")
  [ -n "${install_expected}" ] \
    || fail "the release checksum file is malformed or names another artifact"
  install_actual=$(sha256_file "${install_download}")
  [ "${install_actual}" = "${install_expected}" ] \
    || fail "release checksum mismatch; the existing binary was not changed"

  install_directory=$(select_install_directory)
  publish_binary \
    "${install_download}" "${install_directory}" "${install_expected}"
  printf 'installed argmax %s at %s\n' \
    "${install_published_version}" "${install_directory}/${PROGRAM_NAME}"
  case ":${PATH:-}:" in
    *:"${install_directory}":*) ;;
    *)
      install_quoted_directory=$(shell_quote "${install_directory}") \
        || fail "could not format the PATH update command"
      # shellcheck disable=SC2016
      printf 'argmax is not on PATH; run:\n  export PATH=%s:"$PATH"\n' \
        "${install_quoted_directory}"
      ;;
  esac
  install_quoted_binary=$(
    shell_quote "${install_directory}/${PROGRAM_NAME}"
  ) || fail "could not format the setup command"
  printf 'finish shell setup with:\n  %s setup\n' "${install_quoted_binary}"
}

main "$@"
