// Copies the addon cargo built (a .dll, .so or .dylib) to native/criogen.node.
const fs = require("fs");
const path = require("path");

const target = process.env.CARGO_TARGET_DIR || path.join(__dirname, "..", "target");
const profile = process.argv[2] || "release";
const names = { win32: "criogen_node.dll", darwin: "libcriogen_node.dylib" };
const built = path.join(target, profile, names[process.platform] || "libcriogen_node.so");
const out = path.join(__dirname, "..", "native", "criogen.node");
fs.mkdirSync(path.dirname(out), { recursive: true });
fs.copyFileSync(built, out);
console.log(`copied ${built} -> ${out}`);
