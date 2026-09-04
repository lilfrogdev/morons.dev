#!/usr/bin/env bash
set -euo pipefail

fail() {
    printf 'package-release: %s\n' "$*" >&2
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

verify_binary_target() {
    local path=$1
    local target=$2
    local description
    description=$(LC_ALL=C file -b "$path")
    case "$target:$description" in
        x86_64-apple-darwin:*Mach-O*64-bit*x86_64*) ;;
        aarch64-apple-darwin:*Mach-O*64-bit*arm64*) ;;
        x86_64-unknown-linux-gnu:*ELF*64-bit*x86-64*) ;;
        aarch64-unknown-linux-gnu:*ELF*64-bit*ARM*aarch64*) ;;
        x86_64-pc-windows-msvc:*PE32+*x86-64*) ;;
        aarch64-pc-windows-msvc:*PE32+*ARM64*) ;;
        *) fail "binary format does not match $target: $description" ;;
    esac
}

root=$(git rev-parse --show-toplevel 2>/dev/null) || fail "run from the Morons checkout"
cd "$root"
[[ -z "$(git status --porcelain=v1)" ]] || fail "source checkout must be clean"

host=$(rustc -vV | awk '/^host: / { print $2 }')
target=${1:-$host}
output_dir=${2:-dist}
case "$target" in
    x86_64-apple-darwin \
    | aarch64-apple-darwin \
    | x86_64-unknown-linux-gnu \
    | aarch64-unknown-linux-gnu \
    | x86_64-pc-windows-msvc \
    | aarch64-pc-windows-msvc) ;;
    *) fail "unsupported release target: $target" ;;
esac

version=$(awk -F '"' '/^version = "/ { print $2; exit }' Cargo.toml)
[[ -n "$version" ]] || fail "workspace version is unavailable"
commit=$(git rev-parse --verify HEAD)
[[ -n "$commit" ]] || fail "source commit is unavailable"

case "$target" in
    *-windows-msvc) executable_suffix=.exe ;;
    *) executable_suffix= ;;
esac

cargo build --locked --release --target "$target" -p morons-cli -p morons-server

client="target/$target/release/morons$executable_suffix"
server="target/$target/release/morons-server$executable_suffix"
for binary in "$client" "$server"; do
    [[ -f "$binary" && ! -L "$binary" ]] || fail "missing regular release binary: $binary"
    verify_binary_target "$binary" "$target"
done

package_name="morons-v$version-$target"
stage="$output_dir/$package_name"
archive="$output_dir/$package_name.tar.gz"
checksum="$archive.sha256"
rm -rf -- "$stage"
rm -f -- "$archive" "$checksum"
mkdir -p "$stage"
install -m 755 "$client" "$stage/morons$executable_suffix"
install -m 755 "$server" "$stage/morons-server$executable_suffix"
install -m 644 README.md LICENSE "$stage/"

client_sha=$(sha256_file "$stage/morons$executable_suffix")
server_sha=$(sha256_file "$stage/morons-server$executable_suffix")
cat >"$stage/MANIFEST.txt" <<MANIFEST
name=morons.dev
version=$version
target=$target
commit=$commit
morons_sha256=$client_sha
morons_server_sha256=$server_sha
MANIFEST

COPYFILE_DISABLE=1 tar -C "$output_dir" -czf "$archive" "$package_name"
archive_sha=$(sha256_file "$archive")
printf '%s  %s\n' "$archive_sha" "$(basename "$archive")" >"$checksum"

printf '%s\n' "$archive"
printf '%s\n' "$checksum"
