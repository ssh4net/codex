#!/usr/bin/env bash

set -euo pipefail

# Build and install a local Codex package.
#
# Defaults:
#   source root:  $PWD
#   Codex home:   $HOME/.codex
#   command dir:  $HOME/.local/bin
#
# Optional overrides:
#   CODEX_SOURCE_ROOT, CODEX_TARGET, CODEX_HOME, CODEX_INSTALL_DIR
#   CODEX_PACKAGE_VERSION, CODEX_FORCE_PACKAGE, CODEX_PYTHON, CODEX_RG_BIN
#   CODEX_ZSH_BIN, CODEX_V8_CACHE_DIR

source_root="${CODEX_SOURCE_ROOT:-$PWD}"
source_root="$(cd "$source_root" && pwd -P)"
codex_rs="$source_root/codex-rs"
codex_home="${CODEX_HOME:-$HOME/.codex}"
install_dir="${CODEX_INSTALL_DIR:-$HOME/.local/bin}"
python_bin="${CODEX_PYTHON:-python3}"
cargo_bin="${CARGO:-cargo}"
v8_cache_dir="${CODEX_V8_CACHE_DIR:-$HOME/.cache/codex/rusty-v8}"

require_command() {
    command -v "$1" >/dev/null 2>&1 || {
        printf 'Required command not found: %s\n' "$1" >&2
        exit 1
    }
}

require_command awk
require_command git
require_command rustc
require_command "$cargo_bin"
require_command "$python_bin"

if [[ ! -d "$codex_rs" ]]; then
    printf 'Missing codex-rs directory under source root: %s\n' "$source_root" >&2
    exit 1
fi

host_target="$(rustc -vV | awk '$1 == "host:" {print $2}')"
if [[ -z "$host_target" ]]; then
    printf 'Unable to determine the Rust host target.\n' >&2
    exit 1
fi
target="${CODEX_TARGET:-$host_target}"

if [[ "$target" != *-unknown-linux-* ]]; then
    printf 'This local installer currently supports Linux package targets only: %s\n' "$target" >&2
    printf 'Set CODEX_TARGET to x86_64-unknown-linux-gnu or another Linux target.\n' >&2
    exit 1
fi

short_commit="$(git -C "$source_root" rev-parse --short HEAD)"
package_version="${CODEX_PACKAGE_VERSION:-0.0.0-fork.${short_commit}.$(date -u +%Y%m%d%H%M%S)}"
release_root="$codex_home/packages/standalone"
release_dir="$release_root/releases/${package_version}-${target}"
output_dir="$codex_rs/target/release"

if [[ "$target" != "$host_target" ]]; then
    output_dir="$codex_rs/target/$target/release"
fi

unset V8_FROM_SOURCE RUSTY_V8_ARCHIVE RUSTY_V8_SRC_BINDING_PATH

v8_exports="$(
    cd "$source_root"
    CODEX_REPO_ROOT="$source_root" \
    CODEX_TARGET="$target" \
    CODEX_V8_CACHE_DIR="$v8_cache_dir" \
    "$python_bin" -c '
import os
import shlex
from pathlib import Path

from scripts.codex_package.targets import TARGET_SPECS
from scripts.codex_package.v8 import resolve_codex_v8_cargo_env

target = os.environ["CODEX_TARGET"]
env = resolve_codex_v8_cargo_env(
    TARGET_SPECS[target],
    cache_root=Path(os.environ["CODEX_V8_CACHE_DIR"]),
)
print("\n".join(
    f"export {key}={shlex.quote(value)}"
    for key, value in env.items()
))
'
)"
if [[ -n "$v8_exports" ]]; then
    eval "$v8_exports"
fi

build_args=(build --release --locked --bin codex --bin codex-code-mode-host --bin bwrap)
if [[ "$target" != "$host_target" ]]; then
    build_args=(build --target "$target" --release --locked --bin codex --bin codex-code-mode-host --bin bwrap)
fi

printf 'Building Codex for %s...\n' "$target"
(
    cd "$codex_rs"
    "$cargo_bin" "${build_args[@]}"
)

entrypoint_bin="$output_dir/codex"
code_mode_host_bin="$output_dir/codex-code-mode-host"
bwrap_bin="$output_dir/bwrap"
for binary in "$entrypoint_bin" "$code_mode_host_bin" "$bwrap_bin"; do
    if [[ ! -x "$binary" ]]; then
        printf 'Expected executable was not built: %s\n' "$binary" >&2
        exit 1
    fi
done

package_args=(
    --target "$target"
    --variant codex
    --package-version "$package_version"
    --cargo-profile release
    --package-dir "$release_dir"
    --entrypoint-bin "$entrypoint_bin"
    --code-mode-host-bin "$code_mode_host_bin"
    --bwrap-bin "$bwrap_bin"
)

rg_bin="${CODEX_RG_BIN:-}"
if [[ -z "$rg_bin" ]]; then
    rg_bin="$(command -v rg 2>/dev/null || true)"
fi
if [[ -n "$rg_bin" ]]; then
    package_args+=(--rg-bin "$rg_bin")
fi

if [[ -n "${CODEX_ZSH_BIN:-}" ]]; then
    package_args+=(--zsh-bin "$CODEX_ZSH_BIN")
fi

if [[ "${CODEX_FORCE_PACKAGE:-0}" == "1" ]]; then
    package_args+=(--force)
fi

printf 'Assembling package at %s...\n' "$release_dir"
(
    cd "$source_root"
    CODEX_REPO_ROOT="$source_root" "$python_bin" \
        "$source_root/scripts/build_codex_package.py" \
        "${package_args[@]}"
)

for link_path in "$release_root/current" "$install_dir/codex"; do
    if [[ -e "$link_path" && ! -L "$link_path" ]]; then
        printf 'Refusing to replace a non-symlink: %s\n' "$link_path" >&2
        exit 1
    fi
done

mkdir -p "$release_root" "$install_dir"
ln -sfn bin/codex "$release_dir/codex"
ln -sfn "$release_dir" "$release_root/current"
ln -sfn "$release_root/current/bin/codex" "$install_dir/codex"

"$install_dir/codex" --version

case ":${PATH}:" in
    *":${install_dir}:"*)
        ;;
    *)
        printf 'Add this directory to PATH in your shell profile: %s\n' "$install_dir"
        ;;
esac

printf 'Installed Codex package: %s\n' "$release_dir"
printf 'Command link: %s\n' "$install_dir/codex"
printf 'Run `hash -r` in the current shell, then run `codex`.\n'

if [[ -e "$codex_home/packages/app-server-daemon/current" || -L "$codex_home/packages/app-server-daemon/current" ]]; then
    printf 'An existing daemon package was found. To pin this build, run:\n'
    printf '  %s app-server daemon update --from-cli --yes\n' "$install_dir/codex"
fi
