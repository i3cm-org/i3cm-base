#!/usr/bin/env node
// Pack a module repo into a signed package for the i3cm installer.
//
// Usage: pack <typesDir> <moduleRepo> --wasm <component.wasm> --version <x.y.z> --out <dir>
//             [--module <repo>]... [--key <ed25519 private key PEM file> | I3CM_SIGNING_KEY]
//   <out>/<name>-<version>.tar.gz   module.json (the module's part of the catalog), module.wasm,
//                                   i18n/*.ftl, ui/*.html, SHA256SUMS
//   <out>/<name>-<version>.tar.gz.sig   ed25519 signature over the SHA-256 of the package (hex)
//   <out>/<name>-<version>.json     registry entry: name, version, requires, sha256, file
//
// `keygen <dir>` writes signing.pem (private, keep secret) and signing.pub (hex public key).

import crypto from "node:crypto";
import fs from "node:fs";
import path from "node:path";
import zlib from "node:zlib";
import { buildCatalog, moduleRoot } from "./typecat.mjs";

const sha256 = (b) => crypto.createHash("sha256").update(b).digest("hex");

// Minimal ustar writer: regular files only, names below 100 bytes.
function tar(entries) {
  const blocks = [];
  for (const [name, data] of entries) {
    const h = Buffer.alloc(512, 0);
    h.write(name, 0, 100, "utf8");
    h.write("0000644\0", 100);
    h.write("0000000\0", 108);
    h.write("0000000\0", 116);
    h.write(data.length.toString(8).padStart(11, "0") + "\0", 124);
    h.write("00000000000\0", 136); // mtime 0: reproducible packages
    h.write("        ", 148);
    h.write("0", 156);
    h.write("ustar\0", 257);
    h.write("00", 263);
    let sum = 0;
    for (const b of h) sum += b;
    h.write(sum.toString(8).padStart(6, "0") + "\0 ", 148);
    blocks.push(h, data, Buffer.alloc((512 - (data.length % 512)) % 512, 0));
  }
  blocks.push(Buffer.alloc(1024, 0));
  return Buffer.concat(blocks);
}

/** The part of a catalog that belongs to one module. */
export function modulePart(catalog, name) {
  const mod = catalog.modules.find((m) => m.name === name);
  if (!mod) throw new Error(`module ${name} not in catalog`);
  const own = (n) => n.split(":")[0] === name;
  return {
    format: catalog.format,
    modules: [mod],
    types: catalog.types.filter((t) => own(t.name)),
    references: catalog.references.filter((r) => own(r.name)),
    textTypes: catalog.textTypes.filter((t) => own(t.name)),
  };
}

export function pack({ typesDir, repo, wasm, version, out, modules = [], keyPem }) {
  const root = moduleRoot(repo);
  const cat = buildCatalog(typesDir, { modules: [...modules, repo] });
  const name = /name\s*:\s*["']([\w-]+)["']/.exec(fs.readFileSync(path.join(root, "module.ts"), "utf8"))[1];
  const part = modulePart(cat, name);
  const entries = [["module.json", Buffer.from(JSON.stringify(part, null, 1))]];
  if (wasm) entries.push(["module.wasm", fs.readFileSync(wasm)]);
  for (const sub of ["i18n", "ui"]) {
    const d = path.join(root, sub);
    if (fs.existsSync(d)) for (const f of fs.readdirSync(d).sort()) entries.push([`${sub}/${f}`, fs.readFileSync(path.join(d, f))]);
  }
  entries.push(["SHA256SUMS", Buffer.from(entries.map(([n, d]) => `${sha256(d)}  ${n}\n`).join(""))]);
  const pkg = zlib.gzipSync(tar(entries), { level: 9 });
  fs.mkdirSync(out, { recursive: true });
  const file = `${name}-${version}.tar.gz`;
  fs.writeFileSync(path.join(out, file), pkg);
  const digest = sha256(pkg);
  if (keyPem) {
    const sig = crypto.sign(null, Buffer.from(digest, "hex"), crypto.createPrivateKey(keyPem));
    fs.writeFileSync(path.join(out, `${file}.sig`), sig.toString("hex") + "\n");
  }
  const entry = { name, version, requires: part.modules[0].requires, sha256: digest, file };
  fs.writeFileSync(path.join(out, `${name}-${version}.json`), JSON.stringify(entry, null, 1) + "\n");
  return entry;
}

export function keygen(dir) {
  const { privateKey, publicKey } = crypto.generateKeyPairSync("ed25519");
  fs.mkdirSync(dir, { recursive: true });
  fs.writeFileSync(path.join(dir, "signing.pem"), privateKey.export({ type: "pkcs8", format: "pem" }), { mode: 0o600 });
  const raw = publicKey.export({ type: "spki", format: "der" }).subarray(-32);
  fs.writeFileSync(path.join(dir, "signing.pub"), raw.toString("hex") + "\n");
  return raw.toString("hex");
}

if (process.argv[1]?.endsWith("pack.mjs")) {
  const a = process.argv.slice(2);
  if (a[0] === "keygen") {
    console.log(keygen(a[1] ?? "."));
  } else {
    const opt = (k) => { const i = a.indexOf(k); return i >= 0 ? a[i + 1] : undefined; };
    const modules = a.flatMap((x, i) => (x === "--module" ? [a[i + 1]] : []));
    const keyFile = opt("--key");
    const keyPem = keyFile ? fs.readFileSync(keyFile, "utf8") : process.env.I3CM_SIGNING_KEY;
    const known = new Set(["--wasm", "--version", "--out", "--module", "--key"]);
    for (let i = 2; i < a.length; i += 2) if (!known.has(a[i])) { console.error(`pack: unknown option ${a[i]}`); process.exit(2); }
    const e = pack({ typesDir: a[0], repo: a[1], wasm: opt("--wasm"), version: opt("--version") ?? "0.0.0", out: opt("--out") ?? ".", modules, keyPem });
    console.log(JSON.stringify(e));
  }
}
