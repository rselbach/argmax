#!/bin/sh
# Package one built argmax target as a verified release asset.

set -eu

release_temp_dir=

fail() {
  printf 'argmax release: %s\n' "$*" >&2
  exit 1
}

text_is_safe() {
  [ -n "$1" ] || return 1
  release_sanitized=$(
    printf '%s' "$1" | LC_ALL=C tr -d '\001-\037\177'
  ) || return 1
  [ "${release_sanitized}" = "$1" ]
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

cleanup() {
  if [ -n "${release_temp_dir}" ]; then
    case "${release_temp_dir}" in
      */.argmax-release.*) rm -rf "${release_temp_dir}" ;;
    esac
  fi
}

path_metadata() {
  release_metadata_path=$1
  if stat -c '%u %a' "${release_metadata_path}" >/dev/null 2>&1; then
    release_metadata=$(stat -c '%u %a' "${release_metadata_path}")
  elif stat -f '%u %p' "${release_metadata_path}" >/dev/null 2>&1; then
    release_metadata=$(stat -f '%u %p' "${release_metadata_path}")
  else
    return 1
  fi
  release_path_owner=${release_metadata%% *}
  release_path_mode=${release_metadata#* }
  case "${release_path_owner}" in
    '' | *[!0-9]*) return 1 ;;
  esac
  case "${release_path_mode}" in
    '' | *[!0-7]*) return 1 ;;
  esac
  # BSD %p includes file-type bits; all permission checks use bit masks.
  [ "${#release_path_mode}" -le 6 ] || return 1
  release_path_mode_value=$((0${release_path_mode}))
}

path_has_no_acl() {
  release_acl_path=$1
  # shellcheck disable=SC2012
  release_permissions=$(LC_ALL=C ls -ld "${release_acl_path}" | awk '{ print $1 }') \
    || return 1
  [ -n "${release_permissions}" ] || return 1
  case "${release_permissions}" in
    *+*) return 1 ;;
  esac
}

directory_is_private() {
  release_private_directory=$1
  path_metadata "${release_private_directory}" || return 1
  [ "${release_path_owner}" = "$(id -u)" ] || return 1
  [ $((release_path_mode_value & 0022)) -eq 0 ] || return 1
  path_has_no_acl "${release_private_directory}"
}

path_entry_is_protected() {
  release_entry_parent=$1
  release_entry_child=$2
  path_metadata "${release_entry_parent}" || return 1
  path_has_no_acl "${release_entry_parent}" || return 1
  release_parent_mode_value=${release_path_mode_value}
  [ $((release_parent_mode_value & 0022)) -ne 0 ] || return 0
  [ $((release_parent_mode_value & 01000)) -ne 0 ] || return 1

  # Under a sticky bit only the entry's own owner, the directory's owner, or
  # root may rename or remove it. Accepting a parent owned by the current user
  # while the entry belongs to someone else would leave that other owner able
  # to replace the component after this check.
  path_metadata "${release_entry_child}" || return 1
  release_current_uid=$(id -u) || return 1
  [ "${release_path_owner}" = "${release_current_uid}" ]
}

path_chain_is_secure() {
  release_chain_path=$1
  case "${release_chain_path}" in
    /*) ;;
    *) return 1 ;;
  esac
  [ "${release_chain_path}" != / ] || return 0

  release_chain_current=/
  release_chain_remaining=${release_chain_path#/}
  while [ -n "${release_chain_remaining}" ]; do
    case "${release_chain_remaining}" in
      */*)
        release_chain_component=${release_chain_remaining%%/*}
        release_chain_remaining=${release_chain_remaining#*/}
        ;;
      *)
        release_chain_component=${release_chain_remaining}
        release_chain_remaining=
        ;;
    esac
    [ -n "${release_chain_component}" ] || return 1
    release_chain_parent=${release_chain_current}
    if [ "${release_chain_current}" = / ]; then
      release_chain_current=/${release_chain_component}
    else
      release_chain_current=${release_chain_current}/${release_chain_component}
    fi
    [ -d "${release_chain_current}" ] \
      && [ ! -L "${release_chain_current}" ] \
      || return 1
    path_entry_is_protected \
      "${release_chain_parent}" "${release_chain_current}" \
      || return 1
  done
}

sha256_file() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | awk '{ print $1 }'
    return
  fi
  if command -v shasum >/dev/null 2>&1; then
    shasum -a 256 "$1" | awk '{ print $1 }'
    return
  fi
  fail "sha256sum or shasum is required"
}

main() {
  trap cleanup EXIT HUP INT TERM
  [ "$#" -eq 4 ] \
    || fail "usage: package-release.sh <target> <asset> <version> <output-dir>"
  release_target=$1
  release_asset=$2
  release_version=$3
  release_output=$4
  case "${release_target}:${release_asset}" in
    x86_64-unknown-linux-musl:argmax-linux-amd64 \
      | aarch64-unknown-linux-musl:argmax-linux-arm64 \
      | x86_64-apple-darwin:argmax-macos-amd64 \
      | aarch64-apple-darwin:argmax-macos-arm64) ;;
    *) fail "release target and asset name do not match" ;;
  esac
  case "${release_output}" in
    /*) ;;
    *) fail "release output directory must be absolute" ;;
  esac
  text_is_safe "${release_output}" \
    || fail "release output directory contains unsupported control text"

  release_binary="target/${release_target}/release/argmax"
  if [ ! -f "${release_binary}" ] || [ ! -x "${release_binary}" ]; then
    fail "built executable is missing: ${release_binary}"
  fi
  release_reported=$("${release_binary}" version) \
    || fail "built executable did not report its version"
  is_semantic_version "${release_reported}" \
    || fail "built executable reported an invalid semantic version"
  is_semantic_version "${release_version}" \
    || fail "requested release version is not semantic"
  [ "${release_reported}" = "${release_version}" ] \
    || fail "built version ${release_reported} does not match ${release_version}"

  (umask 077 && mkdir -p "${release_output}") \
    || fail "could not create release output directory"
  release_output=$(CDPATH='' cd -P "${release_output}" && pwd) \
    || fail "could not resolve release output directory"
  text_is_safe "${release_output}" \
    || fail "resolved release output contains unsupported control text"
  directory_is_private "${release_output}" \
    || fail "release output directory is not private to the current user"
  path_chain_is_secure "${release_output}" \
    || fail "an ancestor of the release output directory is not secure"
  release_destination="${release_output}/${release_asset}"
  [ ! -d "${release_destination}" ] \
    || fail "release asset target is a directory"
  [ ! -d "${release_destination}.sha256" ] \
    || fail "release checksum target is a directory"
  release_temp_dir=$(mktemp -d "${release_output}/.argmax-release.XXXXXX") \
    || fail "could not create private release transaction"
  chmod 0700 "${release_temp_dir}" \
    || fail "could not secure release transaction"
  release_temporary="${release_temp_dir}/${release_asset}"
  release_temporary_checksum="${release_temporary}.sha256"
  cp "${release_binary}" "${release_temporary}" \
    || fail "could not copy release executable"
  chmod 0755 "${release_temporary}" \
    || fail "could not set release executable permissions"
  [ "$("${release_temporary}" version)" = "${release_version}" ] \
    || fail "copied release executable failed validation"
  release_hash=$(sha256_file "${release_temporary}")
  printf '%s  %s\n' "${release_hash}" "${release_asset}" \
    >"${release_temporary_checksum}"
  chmod 0644 "${release_temporary_checksum}" \
    || fail "could not set checksum permissions"
  mv -f "${release_temporary}" "${release_destination}" \
    || fail "could not publish release asset"
  mv -f "${release_temporary_checksum}" "${release_destination}.sha256" \
    || fail "could not publish release checksum"
  rmdir "${release_temp_dir}" \
    || fail "could not finish release transaction"
  release_temp_dir=
}

main "$@"
