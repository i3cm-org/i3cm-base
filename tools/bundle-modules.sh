#!/bin/sh
# Bundle the mandatory modules into the kernel image: for each "name repo ref" line of
# deploy/modules/SOURCES, export the repo at ref, take the module's part of the catalog and its
# UI templates into deploy/modules/. The server loads deploy/modules (I3CM_MODULES_DIR) at start;
# tenants get mandatory modules activated at creation.
# Usage: tools/bundle-modules.sh   (run from the repository root; needs node and git)
set -eu
root=$(pwd)
out=deploy/modules
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
mods=""
while read -r name repo ref; do
  case "$name" in ''|'#'*) continue ;; esac
  mkdir -p "$work/$name"
  git -C "$repo" archive "$ref" | tar -x -C "$work/$name"
  mods="$mods --module $work/$name"
done < "$out/SOURCES"
# shellcheck disable=SC2086
node tools/typecat/typecat.mjs types $mods --out "$work/catalog.json" --ui-out "$out/ui" >/dev/null
while read -r name repo ref; do
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
  echo "$name $(git -C "$repo" rev-parse --short "$ref")"
done < "$out/SOURCES"
