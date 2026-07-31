#!/bin/sh
# Exercise verified installer publication and hostile failures without network access.

set -eu

test_root=
test_tmp_base=

fail() {
  printf 'installer test: %s\n' "$*" >&2
  exit 1
}

cleanup() {
  if [ -n "${test_root}" ]; then
    case "${test_root}" in
      "${test_tmp_base}"/argmax-installer-test.*)
        rm -rf "${test_root}"
        ;;
    esac
  fi
}

asset_suffix() {
  case "$(uname -s):$(uname -m)" in
    Linux:x86_64 | Linux:amd64) printf '%s\n' linux-amd64 ;;
    Linux:aarch64 | Linux:arm64) printf '%s\n' linux-arm64 ;;
    Darwin:x86_64 | Darwin:amd64) printf '%s\n' macos-amd64 ;;
    Darwin:aarch64 | Darwin:arm64) printf '%s\n' macos-arm64 ;;
    *) fail "unsupported test host" ;;
  esac
}

sha256_file() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | awk '{ print $1 }'
    return
  fi
  shasum -a 256 "$1" | awk '{ print $1 }'
}

shell_quote() {
  test_quote_value=$1
  printf "'"
  while :; do
    case "${test_quote_value}" in
      *"'"*)
        test_quote_prefix=${test_quote_value%%"'"*}
        printf "%s'\\\\''" "${test_quote_prefix}"
        test_quote_value=${test_quote_value#*"'"}
        ;;
      *)
        printf "%s'" "${test_quote_value}"
        break
        ;;
    esac
  done
}

write_fake_curl() {
  test_path=$1
  {
    printf '%s\n' '#!/bin/sh'
    printf '%s\n' 'set -eu'
    printf '%s\n' 'destination='
    printf '%s\n' 'url='
    printf '%s\n' 'while [ "$#" -gt 0 ]; do'
    printf '%s\n' "  case \"\$1\" in"
    printf '%s\n' "    --output) destination=\$2; shift 2 ;;"
    printf '%s\n' \
      '    --connect-timeout | --max-time | --proto | --proto-redir) shift 2 ;;'
    printf '%s\n' '    --fail | --location | --silent | --show-error | --tlsv1.2) shift ;;'
    printf '%s\n' "    https://*) url=\$1; shift ;;"
    printf '%s\n' '    *) exit 64 ;;'
    printf '%s\n' '  esac'
    printf '%s\n' 'done'
    # shellcheck disable=SC2016
    printf '%s\n' 'if [ "${ARGMAX_TEST_CURL_MODE:-copy}" = fail ]; then'
    # shellcheck disable=SC2016
    printf '%s\n' '  printf "%s\n" partial-download >"${destination}"'
    printf '%s\n' '  exit 22'
    printf '%s\n' 'fi'
    # shellcheck disable=SC2016
    printf '%s\n' \
      'cp "${ARGMAX_TEST_ASSET_DIR}/${url##*/}" "${destination}"'
  } >"${test_path}"
  chmod 0755 "${test_path}"
}

write_fake_argmax() {
  test_path=$1
  {
    printf '%s\n' '#!/bin/sh'
    # shellcheck disable=SC2016
    printf '%s\n' 'if [ "${1-}" = version ]; then'
    # shellcheck disable=SC2016
    printf '%s\n' \
      '  printf "%s\n" "${ARGMAX_TEST_BINARY_VERSION:-9.8.7}"'
    printf '%s\n' '  exit 0'
    printf '%s\n' 'fi'
    printf '%s\n' 'exit 64'
  } >"${test_path}"
  chmod 0755 "${test_path}"
}

prepare_valid_assets() {
  write_fake_argmax "${test_assets}/${test_asset}"
  test_hash=$(sha256_file "${test_assets}/${test_asset}")
  printf '%s  %s\n' "${test_hash}" "${test_asset}" \
    >"${test_assets}/${test_asset}.sha256"
}

reset_run() {
  test_install_dir=${test_bin}
  test_run_home=${test_home}
  test_run_tmp=${test_tmp}
  test_requested_version=9.8.7
  test_reported_version=9.8.7
  test_channel=stable
  test_curl_mode=copy
  test_release_base=https://releases.invalid/download
}

run_installer_command() {
  ARGMAX_TEST_ASSET_DIR=${test_assets} \
    ARGMAX_TEST_CURL_MODE=${test_curl_mode} \
    ARGMAX_TEST_BINARY_VERSION=${test_reported_version} \
    ARGMAX_RELEASE_BASE_URL=${test_release_base} \
    ARGMAX_INSTALL_DIR=${test_install_dir} \
    ARGMAX_VERSION=${test_requested_version} \
    ARGMAX_CHANNEL=${test_channel} \
    HOME=${test_run_home} \
    TMPDIR=${test_run_tmp} \
    PATH="${test_tools}:${test_system_path}" \
    "${test_shell}" "$@"
}

run_installer() {
  run_installer_command "${test_installer}"
}

run_installer_from_stdin() {
  run_installer_command <"${test_installer}"
}

write_old_binary() {
  test_old_path=$1
  printf '%s\n' working-old-binary >"${test_old_path}"
  chmod 0755 "${test_old_path}"
}

assert_old_binary() {
  [ "$(sed -n '1p' "$1")" = working-old-binary ] \
    || fail "$2 replaced the working binary"
}

assert_no_installer_temps() {
  test_temp_parent=$1
  set -- "${test_temp_parent}"/argmax-install.*
  [ "$1" = "${test_temp_parent}/argmax-install.*" ] \
    || fail "installer temporary directory was not removed"
}

assert_no_staged_binary() {
  test_stage_parent=$1
  set -- "${test_stage_parent}"/.argmax-install.*
  [ "$1" = "${test_stage_parent}/.argmax-install.*" ] \
    || fail "staged installation file was not removed"
}

test_success_and_idempotence() {
  reset_run
  test_output=${test_root}/success.out
  run_installer_from_stdin >"${test_output}"
  [ "$("${test_bin}/argmax" version)" = 9.8.7 ] \
    || fail "verified binary was not installed"
  test_quoted_bin=$(shell_quote "${test_bin}")
  grep -F "  export PATH=${test_quoted_bin}:\"\$PATH\"" \
    "${test_output}" >/dev/null \
    || fail "installer did not print the exact PATH correction"
  test_quoted_binary=$(shell_quote "${test_bin}/argmax")
  grep -F "  ${test_quoted_binary} setup" "${test_output}" >/dev/null \
    || fail "installer did not print the exact shell setup command"

  run_installer >/dev/null
  [ "$("${test_bin}/argmax" version)" = 9.8.7 ] \
    || fail "re-running the installer was not idempotent"
  assert_no_installer_temps "${test_tmp}"
  assert_no_staged_binary "${test_bin}"
}

test_download_and_validation_failures() {
  reset_run
  write_old_binary "${test_bin}/argmax"
  printf '%064d  %s\n' 0 "${test_asset}" \
    >"${test_assets}/${test_asset}.sha256"
  if run_installer >/dev/null 2>&1; then
    fail "checksum mismatch unexpectedly succeeded"
  fi
  assert_old_binary "${test_bin}/argmax" "checksum failure"

  prepare_valid_assets
  test_hash=$(sha256_file "${test_assets}/${test_asset}")
  printf '%s  %s\n' "${test_hash}" another-asset \
    >"${test_assets}/${test_asset}.sha256"
  if run_installer >/dev/null 2>&1; then
    fail "checksum naming mismatch unexpectedly succeeded"
  fi
  assert_old_binary "${test_bin}/argmax" "malformed checksum"

  prepare_valid_assets
  test_curl_mode=fail
  if run_installer >/dev/null 2>&1; then
    fail "downloader failure unexpectedly succeeded"
  fi
  assert_old_binary "${test_bin}/argmax" "downloader failure"
  assert_no_installer_temps "${test_tmp}"

  prepare_valid_assets
  test_curl_mode=copy
  test_reported_version=not-semver
  if run_installer >/dev/null 2>&1; then
    fail "malformed binary version unexpectedly succeeded"
  fi
  assert_old_binary "${test_bin}/argmax" "malformed binary version"

  test_reported_version=9.8.6
  if run_installer >/dev/null 2>&1; then
    fail "wrong binary version unexpectedly succeeded"
  fi
  assert_old_binary "${test_bin}/argmax" "wrong binary version"

  test_reported_version='9.8.7
injected'
  if run_installer >/dev/null 2>&1; then
    fail "multiline binary version unexpectedly succeeded"
  fi
  assert_old_binary "${test_bin}/argmax" "multiline binary version"

  reset_run
  test_release_base=http://releases.invalid/download
  if run_installer >/dev/null 2>&1; then
    fail "insecure release URL unexpectedly succeeded"
  fi
  assert_old_binary "${test_bin}/argmax" "insecure release URL"

  reset_run
  test_requested_version=v
  if run_installer >/dev/null 2>&1; then
    fail "empty v-prefixed version unexpectedly succeeded"
  fi
  assert_old_binary "${test_bin}/argmax" "empty v-prefixed version"
}

test_destination_fallbacks() {
  prepare_valid_assets
  reset_run
  test_requested_file=${test_root}/requested-file
  test_file_home=${test_root}/file-home
  printf '%s\n' do-not-replace >"${test_requested_file}"
  mkdir "${test_file_home}"
  chmod 0700 "${test_file_home}"
  test_install_dir=${test_requested_file}
  test_run_home=${test_file_home}
  run_installer >/dev/null
  [ "$(sed -n '1p' "${test_requested_file}")" = do-not-replace ] \
    || fail "requested non-directory path was replaced"
  [ "$("${test_file_home}/.local/bin/argmax" version)" = 9.8.7 ] \
    || fail "requested non-directory path did not fall back"

  reset_run
  test_unsafe_parent=${test_root}/unsafe-parent
  test_private_child=${test_unsafe_parent}/private-bin
  test_ancestor_home=${test_root}/ancestor-home
  mkdir "${test_unsafe_parent}" "${test_ancestor_home}"
  mkdir "${test_private_child}"
  chmod 0777 "${test_unsafe_parent}"
  chmod 0700 "${test_private_child}" "${test_ancestor_home}"
  test_install_dir=${test_private_child}
  test_run_home=${test_ancestor_home}
  run_installer >/dev/null
  [ ! -e "${test_private_child}/argmax" ] \
    || fail "installer used a directory beneath an unsafe ancestor"
  [ "$("${test_ancestor_home}/.local/bin/argmax" version)" = 9.8.7 ] \
    || fail "unsafe ancestor did not trigger the user-local fallback"

  reset_run
  test_public_bin=${test_root}/public-bin
  test_public_home=${test_root}/public-home
  mkdir "${test_public_bin}" "${test_public_home}"
  chmod 0777 "${test_public_bin}"
  chmod 0700 "${test_public_home}"
  test_install_dir=${test_public_bin}
  test_run_home=${test_public_home}
  run_installer >/dev/null
  [ ! -e "${test_public_bin}/argmax" ] \
    || fail "installer used a group- or world-writable directory"
  [ "$("${test_public_home}/.local/bin/argmax" version)" = 9.8.7 ] \
    || fail "unsafe requested directory did not fall back"
}

test_unsafe_tmpdir() {
  prepare_valid_assets
  reset_run
  test_unsafe_tmp=${test_root}/unsafe-tmp
  mkdir "${test_unsafe_tmp}"
  chmod 0777 "${test_unsafe_tmp}"
  write_old_binary "${test_bin}/argmax"
  test_run_tmp=${test_unsafe_tmp}
  test_error=${test_root}/unsafe-tmp.err
  if run_installer >/dev/null 2>"${test_error}"; then
    fail "unsafe TMPDIR unexpectedly succeeded"
  fi
  grep -F 'TMPDIR does not protect private temporary files' \
    "${test_error}" >/dev/null \
    || fail "unsafe TMPDIR failure was not actionable"
  assert_old_binary "${test_bin}/argmax" "unsafe TMPDIR"
  assert_no_installer_temps "${test_unsafe_tmp}"

  chmod 1777 "${test_unsafe_tmp}"
  run_installer >/dev/null
  [ "$("${test_bin}/argmax" version)" = 9.8.7 ] \
    || fail "sticky TMPDIR did not support a verified installation"
  assert_no_installer_temps "${test_unsafe_tmp}"
}

test_existing_target_directory() {
  prepare_valid_assets
  reset_run
  rm -f "${test_bin}/argmax"
  mkdir "${test_bin}/argmax"
  printf '%s\n' keep >"${test_bin}/argmax/marker"
  if run_installer >/dev/null 2>&1; then
    fail "existing target directory unexpectedly succeeded"
  fi
  [ "$(sed -n '1p' "${test_bin}/argmax/marker")" = keep ] \
    || fail "existing target directory was modified"
  assert_no_staged_binary "${test_bin}"
  rm -rf "${test_bin}/argmax"
}

test_missing_downloaders() {
  reset_run
  test_minimal_tools=${test_root}/minimal-tools
  mkdir "${test_minimal_tools}"
  for test_tool in awk chmod id ls mktemp rm stat tr uname; do
    test_tool_path=$(command -v "${test_tool}") \
      || fail "test host is missing ${test_tool}"
    ln -s "${test_tool_path}" "${test_minimal_tools}/${test_tool}"
  done
  test_error=${test_root}/missing-downloaders.err
  if ARGMAX_RELEASE_BASE_URL=${test_release_base} \
    ARGMAX_INSTALL_DIR=${test_bin} \
    ARGMAX_VERSION=${test_requested_version} \
    HOME=${test_home} \
    TMPDIR=${test_tmp} \
    PATH=${test_minimal_tools} \
    "${test_shell}" "${test_installer}" >/dev/null 2>"${test_error}"; then
    fail "installer without curl or wget unexpectedly succeeded"
  fi
  grep -F 'curl or wget is required to download a release' \
    "${test_error}" >/dev/null \
    || fail "missing downloader failure was not actionable"
  assert_no_installer_temps "${test_tmp}"
}

test_concurrent_publication() {
  prepare_valid_assets
  reset_run
  rm -f "${test_bin}/argmax"
  run_installer >"${test_root}/concurrent-one.out" \
    2>"${test_root}/concurrent-one.err" &
  test_first_pid=$!
  run_installer >"${test_root}/concurrent-two.out" \
    2>"${test_root}/concurrent-two.err" &
  test_second_pid=$!
  wait "${test_first_pid}" \
    || fail "first concurrent installer failed"
  wait "${test_second_pid}" \
    || fail "second concurrent installer failed"
  [ "$("${test_bin}/argmax" version)" = 9.8.7 ] \
    || fail "concurrent installers did not leave a verified executable"
  assert_no_installer_temps "${test_tmp}"
  assert_no_staged_binary "${test_bin}"
}

test_quoted_path_command() {
  prepare_valid_assets
  reset_run
  test_quoted_directory=${test_root}/Troy\'s\ bin
  mkdir "${test_quoted_directory}"
  test_install_dir=${test_quoted_directory}
  test_output=${test_root}/quoted-path.out
  run_installer >"${test_output}"
  test_path_command=$(sed -n '/^  export PATH=/s/^  //p' "${test_output}")
  [ -n "${test_path_command}" ] \
    || fail "quoted install path did not produce a PATH command"
  ARGMAX_EXPECTED_PATH=${test_quoted_directory} \
    "${test_shell}" -c \
    "${test_path_command}; test \"\${PATH%%:*}\" = \"\${ARGMAX_EXPECTED_PATH}\"" \
    || fail "printed PATH command did not preserve the install path"
}

assert_user_state_preserved() {
  [ "$(sed -n '1p' "${test_home}/.config/argmax/config.toml")" = \
    'theme = "Greendale"' ] \
    || fail "installer changed existing configuration"
  [ "$(sed -n '1p' "${test_home}/.zshrc")" = '# existing hook' ] \
    || fail "installer changed existing shell integration"
  [ "$(sed -n '1p' "${test_home}/.local/share/argmax/history")" = \
    'Troy and Abed in the Morning' ] \
    || fail "installer changed learned history"
}

main() {
  trap cleanup EXIT HUP INT TERM
  test_tmp_base=$(CDPATH='' cd -P "${TMPDIR:-/tmp}" && pwd)
  test_root=$(mktemp -d "${test_tmp_base}/argmax-installer-test.XXXXXX")
  test_tools=${test_root}/tools
  test_assets=${test_root}/assets
  test_bin=${test_root}/bin
  test_home=${test_root}/home
  test_tmp=${test_root}/tmp
  mkdir "${test_tools}" "${test_assets}" "${test_bin}" \
    "${test_home}" "${test_tmp}"
  chmod 0700 "${test_root}" "${test_home}" "${test_tmp}"
  mkdir -p "${test_home}/.config/argmax" \
    "${test_home}/.local/share/argmax"
  printf '%s\n' 'theme = "Greendale"' \
    >"${test_home}/.config/argmax/config.toml"
  printf '%s\n' '# existing hook' >"${test_home}/.zshrc"
  printf '%s\n' 'Troy and Abed in the Morning' \
    >"${test_home}/.local/share/argmax/history"

  test_repository=$(CDPATH='' cd "$(dirname "$0")/.." && pwd)
  test_installer=${test_repository}/scripts/install.sh
  test_asset=argmax-$(asset_suffix)
  test_shell=$(command -v sh)
  case "${test_shell}" in
    /*) ;;
    *) fail "could not resolve the system sh" ;;
  esac
  test_system_path=${PATH}
  write_fake_curl "${test_tools}/curl"
  prepare_valid_assets

  test_success_and_idempotence
  test_download_and_validation_failures
  test_destination_fallbacks
  test_unsafe_tmpdir
  test_existing_target_directory
  test_missing_downloaders
  test_concurrent_publication
  test_quoted_path_command
  assert_user_state_preserved
}

main "$@"
