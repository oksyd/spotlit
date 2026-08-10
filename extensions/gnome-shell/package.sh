#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(cd "$script_dir/../.." && pwd)"
source_dir="$script_dir/lock-screen@spotlit.app"
output_dir="$repo_root/target/gnome-extension"
schema="$source_dir/schemas/org.gnome.shell.extensions.spotlit-lock-screen.gschema.xml"

mkdir -p "$output_dir"
glib-compile-schemas --strict --dry-run "$source_dir/schemas"
gnome-extensions pack --force --out-dir "$output_dir" --schema "$schema" "$source_dir"
