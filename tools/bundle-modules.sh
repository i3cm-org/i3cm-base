#!/bin/sh
# Bundle the mandatory modules into the kernel image: for each "name repo ref [package]" line of
# deploy/modules/SOURCES, export the repo at ref, take the module's part of the catalog, its UI
# templates and sources into deploy/modules/, and its code as <name>.wasm when the module has
# code (ops/):
# - with a package (the module's release package <name>-<version>.tar.gz, a path), module.wasm
#   is taken from it after its SHA256SUMS entry matched;
# - without one, ops/ is built at ref (cargo build --release --target wasm32-wasip2).
# The server loads deploy/modules (I3CM_MODULES_DIR) at start; tenants get mandatory modules
# activated at creation.
# Usage: tools/bundle-modules.sh   (run from the repository root; needs node and git, and cargo
# with the wasm32-wasip2 target for modules with code and no package)
set -eu
root=$(pwd)
out=deploy/modules
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
mods=""
while read -r name repo ref pkg; do
  case "$name" in ''|'#'*) continue ;; esac
  mkdir -p "$work/$name"
  git -C "$repo" archive "$ref" | tar -x -C "$work/$name"
  mods="$mods --module $work/$name"
done < "$out/SOURCES"
# shellcheck disable=SC2086
node tools/typecat/typecat.mjs types $mods --out "$work/catalog.json" --ui-out "$out/ui" >/dev/null
while read -r name repo ref pkg; do
  case "$name" in ''|'#'*) continue ;; esac
  node -e '
    const fs = require("fs");
    const c = JSON.parse(fs.readFileSync(process.argv[1], "utf8"));
    const n = process.argv[2];
    const own = (x) => x.split(":")[0] === n;
    const part = { format: c.format, modules: c.modules.filter((m) => m.name === n), types: c.types.filter((t) => own(t.name)),
      references: c.references.filter((r) => own(r.name)), textTypes: c.textTypes.filter((t) => own(t.name)) };
    fs.writeFileSync(process.argv[3], JSON.stringify(part, null, 1) + "\n");
  ' "$work/catalog.json" "$name" "$out/$name.json"
  # sources for checks of dependent modules (studio, GET /v1/modules/{name}/sources)
  rm -rf "$out/src/$name" && mkdir -p "$out/src/$name" && cp -R "$work/$name/i3cm" "$out/src/$name/i3cm"
  # code
  rm -f "$out/$name.wasm"
  if [ -n "${pkg:-}" ]; then
    mkdir -p "$work/pkg-$name"
    tar -xzf "$pkg" -C "$work/pkg-$name"
    if [ -f "$work/pkg-$name/module.wasm" ]; then
      want=$(grep ' module.wasm$' "$work/pkg-$name/SHA256SUMS" | cut -d' ' -f1)
      have=$(sha256sum "$work/pkg-$name/module.wasm" | cut -d' ' -f1)
      [ -n "$want" ] && [ "$want" = "$have" ] || { echo "bundle-modules: $name: module.wasm does not match SHA256SUMS of $pkg" >&2; exit 1; }
      cp "$work/pkg-$name/module.wasm" "$out/$name.wasm"
    fi
  elif [ -f "$work/$name/ops/Cargo.toml" ]; then
    cargo build --quiet --release --target wasm32-wasip2 --manifest-path "$work/$name/ops/Cargo.toml" --target-dir "$work/target-$name"
    wasm=$(find "$work/target-$name/wasm32-wasip2/release" -maxdepth 1 -name '*.wasm' | head -n 1)
    [ -n "$wasm" ] || { echo "bundle-modules: $name: building ops/ produced no component" >&2; exit 1; }
    cp "$wasm" "$out/$name.wasm"
  fi
  echo "$name $(git -C "$repo" rev-parse --short "$ref")$( [ -f "$out/$name.wasm" ] && echo ' (with code)')"
done < "$out/SOURCES"
