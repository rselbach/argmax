#!/bin/sh
set -eu

if [ "$#" -ne 3 ]; then
  echo "usage: render-aur.sh <version> <checksums.txt> <output-directory>" >&2
  exit 2
fi

version=$1
checksums=$2
output=$3

if ! printf '%s\n' "$version" | grep -Eq '^[0-9]+\.[0-9]+\.[0-9]+$'; then
  echo "render-aur: invalid stable release version: $version" >&2
  exit 1
fi

archive="argmax-${version}-source.tar.gz"
checksum=$(awk -v archive="$archive" '
  $2 == archive { checksum = $1; matches++ }
  END {
    if (matches != 1) exit 1
    print checksum
  }
' "$checksums") || {
  echo "render-aur: expected exactly one checksum for $archive in $checksums" >&2
  exit 1
}
if ! printf '%s\n' "$checksum" | grep -Eq '^[0-9a-fA-F]{64}$'; then
  echo "render-aur: invalid SHA-256 for $archive in $checksums" >&2
  exit 1
fi

root=$(
  unset CDPATH
  cd -- "$(dirname -- "$0")/.."
  pwd
)
mkdir -p "$output"
for name in PKGBUILD .SRCINFO; do
  sed \
    -e "s/@PKGVER@/$version/g" \
    -e "s/@SOURCE_SHA256@/$checksum/g" \
    "$root/packaging/aur/$name" >"$output/$name"
done

if grep -Eq '@(PKGVER|SOURCE_SHA256)@' "$output/PKGBUILD" "$output/.SRCINFO"; then
  echo "render-aur: unresolved template placeholder" >&2
  exit 1
fi
