#!/usr/bin/env bash
set -euo pipefail

fail() {
    printf 'fetch-uv: %s\n' "$*" >&2
    exit 1
}

sha256_file() {
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum "$1" | awk '{print $1}'
    elif command -v shasum >/dev/null 2>&1; then
        shasum -a 256 "$1" | awk '{print $1}'
    else
        fail "sha256sum or shasum is required"
    fi
}

[[ $# -eq 2 ]] || fail "usage: fetch-uv.sh TARGET OUTPUT"
target=$1
output=$2
root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
assets="$root/crates/morons-server/runtime/uv-assets.txt"
[[ -f "$assets" ]] || fail "reviewed uv asset manifest is missing"
version=$(awk '$1 == "version" { print $2 }' "$assets")
entry=$(awk -v target="$target" '$1 == target { print $2 " " $3 " " $4 }' "$assets")
[[ -n "$version" && -n "$entry" ]] || fail "unsupported target: $target"
read -r archive_sha binary_sha extension <<<"$entry"
case "$target" in
    *-windows-msvc) executable=uv.exe ;;
    *) executable=uv ;;
esac

command -v curl >/dev/null 2>&1 || fail "curl is required"
command -v tar >/dev/null 2>&1 || fail "tar is required"

temporary=$(mktemp -d "${TMPDIR:-/tmp}/morons-uv.XXXXXX")
trap 'rm -rf -- "$temporary"' EXIT
archive="uv-$target.$extension"
url="https://github.com/astral-sh/uv/releases/download/$version/$archive"
curl --fail --silent --show-error --location --proto '=https' --tlsv1.2 \
    "$url" -o "$temporary/$archive"
actual_sha=$(sha256_file "$temporary/$archive")
[[ "$actual_sha" = "$archive_sha" ]] || fail "archive checksum mismatch for $target"
tar -xf "$temporary/$archive" -C "$temporary"
if [[ "$extension" = zip ]]; then
    source="$temporary/$executable"
else
    source="$temporary/uv-$target/$executable"
fi
[[ -f "$source" && ! -L "$source" ]] || fail "archive does not contain the expected uv executable"
mkdir -p "$(dirname "$output")"
install -m 755 "$source" "$output"
output_sha=$(sha256_file "$output")
[[ "$output_sha" = "$binary_sha" ]] || fail "executable checksum mismatch for $target"
printf '%s\n' "$output"
