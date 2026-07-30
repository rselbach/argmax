#!/bin/sh
# Exercise release labeling, checksums, and hostile output paths without a Rust build.

set -eu

test_root=
test_tmp_base=

fail() {
  printf 'release packaging test: %s\n' "$*" >&2
  exit 1
}

cleanup() {
  if [ -n "${test_root}" ]; then
    case "${test_root}" in
      "${test_tmp_base}"/argmax-release-test.*)
        rm -rf "${test_root}"
        ;;
    esac
  fi
}

sha256_file() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | awk '{ print $1 }'
    return
  fi
  shasum -a 256 "$1" | awk '{ print $1 }'
}

write_fake_binary() {
  test_path=$1
  {
    printf '%s\n' '#!/bin/sh'
    # shellcheck disable=SC2016
    printf '%s\n' 'test "${1-}" = version || exit 64'
    # shellcheck disable=SC2016
    printf '%s\n' \
      'printf "%s\n" "${ARGMAX_TEST_BINARY_VERSION:-1.2.3-nightly.20260729}"'
  } >"${test_path}"
  chmod 0755 "${test_path}"
}

run_packager() {
  (
    cd "${test_repository}"
    ARGMAX_TEST_BINARY_VERSION=${test_reported_version} \
      "${test_script}" "${test_target}" "${test_asset}" \
      "${test_version}" "${test_output}"
  )
}

reset_run() {
  test_target=x86_64-unknown-linux-musl
  test_asset=argmax-linux-amd64
  test_version=1.2.3-nightly.20260729
  test_reported_version=1.2.3-nightly.20260729
  test_output=${test_root}/output
}

assert_no_transactions() {
  test_transaction_parent=$1
  set -- "${test_transaction_parent}"/.argmax-release.*
  [ "$1" = "${test_transaction_parent}/.argmax-release.*" ] \
    || fail "release transaction directory was not removed"
}

assert_valid_package() {
  test_package_output=$1
  [ "$("${test_package_output}/${test_asset}" version)" = "${test_version}" ] \
    || fail "packaged executable did not retain its version"
  test_hash=$(sha256_file "${test_package_output}/${test_asset}")
  test_checksum_line=$(sed -n '1p' \
    "${test_package_output}/${test_asset}.sha256")
  [ "${test_checksum_line}" = "${test_hash}  ${test_asset}" ] \
    || fail "packaged checksum does not exactly name the executable"
  assert_no_transactions "${test_package_output}"
}

test_success_and_idempotence() {
  reset_run
  run_packager
  assert_valid_package "${test_output}"
  test_first_hash=$(sha256_file "${test_output}/${test_asset}")

  run_packager
  assert_valid_package "${test_output}"
  [ "$(sha256_file "${test_output}/${test_asset}")" = "${test_first_hash}" ] \
    || fail "repackaging changed identical executable bytes"
}

test_label_and_version_failures() {
  reset_run
  test_asset=argmax-linux-arm64
  if run_packager >/dev/null 2>&1; then
    fail "mislabeled target unexpectedly packaged"
  fi

  reset_run
  test_version=01.2.3
  if run_packager >/dev/null 2>&1; then
    fail "malformed requested version unexpectedly packaged"
  fi

  reset_run
  test_reported_version=not-semver
  if run_packager >/dev/null 2>&1; then
    fail "malformed executable version unexpectedly packaged"
  fi

  reset_run
  test_reported_version=1.2.4-nightly.20260729
  if run_packager >/dev/null 2>&1; then
    fail "version-mismatched executable unexpectedly packaged"
  fi
}

test_unsafe_outputs() {
  reset_run
  test_output=${test_root}/public-output
  mkdir "${test_output}"
  chmod 0777 "${test_output}"
  if run_packager >/dev/null 2>&1; then
    fail "group- or world-writable output unexpectedly succeeded"
  fi
  [ ! -e "${test_output}/${test_asset}" ] \
    || fail "unsafe output received a release asset"

  reset_run
  test_unsafe_parent=${test_root}/unsafe-output-parent
  test_output=${test_unsafe_parent}/private-output
  mkdir "${test_unsafe_parent}" "${test_output}"
  chmod 0777 "${test_unsafe_parent}"
  chmod 0700 "${test_output}"
  if run_packager >/dev/null 2>&1; then
    fail "output beneath an unsafe ancestor unexpectedly succeeded"
  fi
  [ ! -e "${test_output}/${test_asset}" ] \
    || fail "output beneath an unsafe ancestor received an asset"
  assert_no_transactions "${test_output}"

  reset_run
  test_output=${test_root}/output-file
  printf '%s\n' keep >"${test_output}"
  if run_packager >/dev/null 2>&1; then
    fail "non-directory output path unexpectedly succeeded"
  fi
  [ "$(sed -n '1p' "${test_output}")" = keep ] \
    || fail "non-directory output path was modified"

  reset_run
  test_output="${test_root}/bad
output"
  if run_packager >/dev/null 2>&1; then
    fail "control-text output path unexpectedly succeeded"
  fi
  [ ! -e "${test_output}" ] \
    || fail "control-text output path was created"
}

test_sticky_ancestor() {
  reset_run
  test_sticky_parent=${test_root}/sticky-parent
  test_output=${test_sticky_parent}/private-output
  mkdir "${test_sticky_parent}" "${test_output}"
  chmod 1777 "${test_sticky_parent}"
  chmod 0700 "${test_output}"
  run_packager
  assert_valid_package "${test_output}"
}

test_existing_target_directories() {
  reset_run
  test_output=${test_root}/directory-target-output
  mkdir "${test_output}"
  chmod 0700 "${test_output}"
  mkdir "${test_output}/${test_asset}"
  printf '%s\n' keep >"${test_output}/${test_asset}/marker"
  if run_packager >/dev/null 2>&1; then
    fail "existing asset directory unexpectedly succeeded"
  fi
  [ "$(sed -n '1p' "${test_output}/${test_asset}/marker")" = keep ] \
    || fail "existing asset directory was modified"

  rm -rf "${test_output:?}/${test_asset}"
  printf '%s\n' previous >"${test_output}/${test_asset}"
  mkdir "${test_output}/${test_asset}.sha256"
  printf '%s\n' keep >"${test_output}/${test_asset}.sha256/marker"
  if run_packager >/dev/null 2>&1; then
    fail "existing checksum directory unexpectedly succeeded"
  fi
  [ "$(sed -n '1p' "${test_output}/${test_asset}")" = previous ] \
    || fail "checksum-directory failure replaced the prior asset"
  [ "$(sed -n '1p' "${test_output}/${test_asset}.sha256/marker")" = keep ] \
    || fail "existing checksum directory was modified"
  assert_no_transactions "${test_output}"
}

main() {
  trap cleanup EXIT HUP INT TERM
  test_tmp_base=$(CDPATH='' cd -P "${TMPDIR:-/tmp}" && pwd)
  test_root=$(mktemp -d "${test_tmp_base}/argmax-release-test.XXXXXX")
  chmod 0700 "${test_root}"
  test_repository=${test_root}/repository
  test_script=$(CDPATH='' cd "$(dirname "$0")/.." && pwd)/scripts/package-release.sh
  test_target=x86_64-unknown-linux-musl
  mkdir -p "${test_repository}/target/${test_target}/release"
  write_fake_binary "${test_repository}/target/${test_target}/release/argmax"

  test_success_and_idempotence
  test_label_and_version_failures
  test_unsafe_outputs
  test_sticky_ancestor
  test_existing_target_directories
}

main "$@"
