// The Rust API reference: rustdoc's own pages for the public crates, copied
// into public/rustdoc (served at /rustdoc/…).
//
// CriogenAI is open source, so rustdoc's source pages (`src/`) and its
// "Source" links are published with the API pages.
import { spawnSync } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";

/** The crates a user can depend on, in the order the page lists them. */
export const RUST_CRATES = ["agent", "core", "providers", "storage", "telemetry", "gateway", "trace", "rt"];

export function rustCrates(repo) {
  return RUST_CRATES.map((short) => {
    const toml = fs.readFileSync(path.join(repo, "crates", `criogenai-${short}`, "Cargo.toml"), "utf8");
    const description = /^description\s*=\s*"([^"]*)"/m.exec(toml)?.[1] ?? "";
    return {
      name: `criogenai-${short}`,
      module: `criogen_${short}`,
      // Design numbers are for the repository's own notes.
      description: description.replace(/\s*\((?:designs?)\s[^)]*\)/gi, ""),
    };
  });
}

function cargo() {
  const exe = process.platform === "win32" ? "cargo.exe" : "cargo";
  const home = path.join(os.homedir(), ".cargo", "bin", exe);
  const found = spawnSync(exe, ["--version"], { stdio: "ignore" });
  return found.status === 0 ? exe : home;
}

export function buildRustDocs(root) {
  const repo = path.resolve(root, "..");
  const crates = rustCrates(repo);
  const targetDir = path.join(repo, "target", "api-doc");
  const args = ["doc", "--no-deps", "--all-features", "--lib", "--target-dir", targetDir];
  for (const c of crates) args.push("-p", c.name);
  console.log(`[api] rustdoc for ${crates.map((c) => c.name).join(", ")}`);
  const run = spawnSync(cargo(), args, { cwd: repo, stdio: "inherit", env: { ...process.env, RUSTDOCFLAGS: "" } });
  if (run.status !== 0) throw new Error("cargo doc failed");

  const from = path.join(targetDir, "doc");
  const to = path.join(root, "public", "rustdoc");
  fs.rmSync(to, { recursive: true, force: true });
  fs.mkdirSync(to, { recursive: true });
  // Everything but the source pages.
  const skip = new Set([".lock"]);
  for (const entry of fs.readdirSync(from, { withFileTypes: true })) {
    if (skip.has(entry.name)) continue;
    fs.cpSync(path.join(from, entry.name), path.join(to, entry.name), { recursive: true });
  }

  const walk = (dir) => {
    for (const entry of fs.readdirSync(dir, { withFileTypes: true })) {
      const full = path.join(dir, entry.name);
      if (entry.isDirectory()) walk(full);
      else if (entry.name.endsWith(".html")) {
        const html = fs.readFileSync(full, "utf8");
        const cleaned = html
          // "(design 0004 §6)": numbers from the repository's own notes, not for readers.
          .replace(/\s*\((?:designs?)\s[^)<]*\)/gi, "")
          .replace(/,\s*\bdesigns?\s+\d{4}(?:\s*§\s*[\d.]+)?(?:,\s*\d{4})*/gi, "");
        if (cleaned !== html) fs.writeFileSync(full, cleaned);
      }
    }
  };
  walk(to);
  console.log("[api] rust docs in public/rustdoc");
  return crates;
}

