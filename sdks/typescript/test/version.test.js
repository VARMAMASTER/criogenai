// The Rust crate, the npm package and the Python package carry one version: a
// release that bumps only one of them fails here.
const test = require("node:test");
const assert = require("node:assert");
const fs = require("node:fs");
const path = require("node:path");

const read = (file) => fs.readFileSync(path.join(__dirname, "..", file), "utf8");
const tomlVersion = (text) => /^version\s*=\s*"([^"]+)"/m.exec(text)?.[1];

// Only in the repository: the package test copies these tests into a new project.
const inRepo = fs.existsSync(path.join(__dirname, "..", "Cargo.toml"));

test("the crate and package versions agree", { skip: !inRepo && "not in the repository" }, () => {
  const npm = JSON.parse(read("package.json")).version;
  assert.strictEqual(tomlVersion(read("Cargo.toml")), npm, "sdks/typescript/Cargo.toml");
  const python = path.join(__dirname, "..", "..", "python");
  // Only in the repository: the installed tarball has no Python sources.
  if (fs.existsSync(path.join(python, "pyproject.toml"))) {
    assert.strictEqual(tomlVersion(fs.readFileSync(path.join(python, "Cargo.toml"), "utf8")), npm, "sdks/python/Cargo.toml");
    assert.strictEqual(tomlVersion(fs.readFileSync(path.join(python, "pyproject.toml"), "utf8")), npm, "sdks/python/pyproject.toml");
  }
});
