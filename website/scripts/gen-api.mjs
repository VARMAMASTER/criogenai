// The API reference for Python, TypeScript and Rust.
//
//   node scripts/gen-api.mjs           writes the Python and TypeScript pages
//                                      (content/docs/reference/api/…), the Rust
//                                      overview page and lib/api-nav.ts
//   node scripts/gen-api.mjs --check   fails if those files are not what the
//                                      sources produce now (used by CI)
//   node scripts/gen-api.mjs --rust    builds rustdoc into public/rustdoc
//
// Python is read from its source with `ast` (scripts/api/python.py), TypeScript
// with the compiler's symbol table (scripts/api/typescript.mjs), Rust with
// rustdoc (scripts/api/rust.mjs). Nothing is imported or run from the SDKs.
import { spawnSync } from "node:child_process";
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { buildRustDocs, rustCrates } from "./api/rust.mjs";
import { extractTypescript } from "./api/typescript.mjs";

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const args = process.argv.slice(2);

// ---------- what goes on which page ----------

/** The reference pages, the same for both languages. */
const PAGES = [
  { slug: "agents", title: "Agents, tools and errors", modules: ["agent", "tools", "hooks", "usage", "errors"] },
  { slug: "models", title: "Models and the gateway", modules: ["models", "gateway", "env", "redact"] },
  { slug: "storage", title: "Storage, memory and collections", modules: ["storage", "collections"] },
  { slug: "workflows", title: "Workflows", modules: ["workflow"] },
  { slug: "patterns", title: "Patterns, specialists, web and MCP", modules: ["patterns", "specialists", "web", "mcp"] },
  { slug: "observability", title: "Tracing, evals and checks", modules: ["telemetry", "trace", "evals", "ready"] },
];

const MODULE_TITLES = {
  agent: "Agents",
  tools: "Tools",
  hooks: "Hooks",
  usage: "Usage and cost",
  errors: "Errors",
  models: "Models",
  gateway: "Gateway",
  env: "Keys and environment",
  redact: "Redaction",
  storage: "Storage and memory",
  collections: "Collections",
  workflow: "Workflows",
  patterns: "Patterns",
  specialists: "Specialist agents",
  web: "Web search and page reading",
  mcp: "MCP servers",
  telemetry: "Telemetry",
  trace: "Trace viewer",
  evals: "Evals",
  ready: "Readiness check",
};

/** Names that lead their module's section (the entry points), in this order; the rest follow in source order. */
const LEAD = {
  agent: ["Agent", "AgentOptions", "Thread", "Stream", "Reply", "RunResult", "RunOptions", "CallOptions", "Decision", "ApprovalRequest"],
  tools: ["tool", "Tool", "ToolDefinition", "ToolError", "ToolContext"],
  errors: ["CriogenError"],
  models: ["openai", "OpenAI", "anthropic", "Anthropic", "gemini", "Gemini", "FakeModel", "OpenAICompatible", "openaiCompatible"],
  gateway: ["Gateway"],
  storage: ["Storage", "Memory", "StorageOptions", "MemoryOptions"],
  collections: ["Collection", "CollectionOptions"],
  workflow: ["Workflow", "CompiledWorkflow", "WorkflowThread", "END"],
  mcp: ["McpServer", "McpOptions", "stdio", "http"],
  telemetry: ["Telemetry", "TelemetryOptions"],
  trace: ["serve", "serveTraces", "TraceViewer"],
  evals: ["evaluate", "Dataset", "EvalResult", "compare", "check"],
  patterns: ["sequential", "parallel", "loop", "custom"],
  ready: ["ready", "ready_sync"],
};

const LANGUAGES = {
  python: { label: "Python", fence: "python", package: "criogen" },
  typescript: { label: "TypeScript", fence: "ts", package: "criogen" },
};

// ---------- prose: documentation text as MDX ----------

/** Docstrings and JSDoc are Markdown-ish text; MDX also reads `{`, `<`, `import` and indented code. */
function prose(raw, fence) {
  let text = String(raw ?? "").replace(/\r\n/g, "\n").trim();
  if (!text) return "";
  text = noDesign(text);
  // reST, as written in the Python docstrings: ":meth:`run`", and double backticks (which may wrap lines).
  text = text.replace(/:[a-z]+:`([^`\n]+)`/g, "`$1`");
  text = text.replace(/``([^`]{1,200}?)``/g, (_, inner) => "`" + inner.replace(/\s*\n\s*/g, " ") + "`");
  return escapeMdx(indentedBlocks(text, fence));
}

/** "(design 0023)" and ", design 0017 §3": numbers from the repository's own notes, not for readers. */
function noDesign(text) {
  return text
    .replace(/\s*\((?:designs?)\s[^)]*\)/gi, "")
    .replace(/,\s*\bdesigns?\s+\d{4}(?:\s*§\s*[\d.]+)?(?:,\s*\d{4})*/gi, "");
}

/** MDX has no indented code blocks: an indented block after a blank line becomes a fence. */
function indentedBlocks(text, fence) {
  const lines = text.split("\n");
  const out = [];
  let open = false;
  const indented = (l) => /^( {4}|\t)/.test(l);
  for (let i = 0; i < lines.length; ) {
    const line = lines[i];
    if (/^\s*```/.test(line)) {
      open = !open;
      out.push(line);
      i += 1;
      continue;
    }
    const afterBlank = i === 0 || lines[i - 1].trim() === "";
    if (!open && afterBlank && indented(line) && line.trim() !== "") {
      const block = [];
      let j = i;
      while (j < lines.length && (indented(lines[j]) || (lines[j].trim() === "" && j + 1 < lines.length && indented(lines[j + 1])))) {
        block.push(lines[j].replace(/^( {4}|\t)/, ""));
        j += 1;
      }
      out.push("```" + fence, ...block, "```");
      i = j;
      continue;
    }
    out.push(line);
    i += 1;
  }
  return out.join("\n");
}

/** Outside code, `{` `}` `<` and lines that MDX would read as imports or headings are escaped. */
function escapeMdx(text) {
  return text
    .split(/(```[\s\S]*?```|`[^`\n]*`)/g)
    .map((part, i) => {
      if (i % 2) return part;
      return part
        .replace(/[{}]/g, (c) => `\\${c}`)
        .replace(/</g, "&lt;")
        .replace(/^(\s*)import(\s)/gm, "$1&#105;mport$2")
        .replace(/^(\s*)export(\s)/gm, "$1&#101;xport$2")
        .replace(/^#/gm, "\\#");
    })
    .join("");
}

const code = (fence, text) => "```" + fence + "\n" + noDesign(text.replace(/\r\n/g, "\n")).trimEnd() + "\n```";

// ---------- one language's pages ----------

const isTopLevel = (item) => item.access.startsWith("from criogen import") || item.access.startsWith("import {");

/** A module's items as shown: tuning constants that only live in a namespace and say nothing are left out. */
function shownItems(module, id) {
  const items = module.items.filter((item) => !(item.kind === "constant" && !item.doc && !isTopLevel(item)));
  const lead = LEAD[id] ?? [];
  const rank = (item) => {
    const at = lead.indexOf(item.name);
    return at === -1 ? lead.length : at;
  };
  // Array.prototype.sort is stable: items of the same rank keep their source order.
  return [...items].sort((a, b) => rank(a) - rank(b));
}

function importBlock(language, id, module, items) {
  const names = items.filter(isTopLevel).map((i) => i.name);
  const lines = [];
  if (language === "python") {
    if (module.namespace) {
      lines.push(`from criogen import ${id}`, "", ...(items.filter((i) => !isTopLevel(i)).slice(0, 1).map((i) => `${id}.${i.name}(...)`)));
    }
    if (names.length) {
      const one = `from criogen import ${names.join(", ")}`;
      lines.push(one.length <= 88 ? one : `from criogen import (\n${names.map((n) => `    ${n},`).join("\n")}\n)`);
    }
  } else {
    if (module.namespace) lines.push(`import { ${id} } from "criogen";`, `${id}.${items.find((i) => !isTopLevel(i))?.name ?? "…"}`);
    if (names.length) {
      const one = `import { ${names.join(", ")} } from "criogen";`;
      lines.push(one.length <= 88 ? one : `import {\n${names.map((n) => `  ${n},`).join("\n")}\n} from "criogen";`);
    }
  }
  return lines.filter((l, i, a) => !(l === "" && (i === 0 || i === a.length - 1))).join("\n");
}

function renderItem(language, item) {
  const fence = LANGUAGES[language].fence;
  const out = [`### ${item.name}`, "", code(fence, item.signature), ""];
  const doc = prose(item.doc, fence);
  if (doc) out.push(doc, "");
  for (const member of item.members) {
    out.push(`#### ${item.name}.${member.name}`, "", code(fence, member.signature), "");
    const text = prose(member.doc, fence);
    if (text) out.push(text, "");
  }
  return out.join("\n");
}

function renderPage(language, page, data) {
  const lang = LANGUAGES[language];
  const body = [];
  const names = [];
  for (const id of page.modules) {
    const module = data.modules[id];
    if (!module) continue;
    const items = shownItems(module, id);
    if (!items.length) continue;
    names.push(...items.map((i) => i.name));
    body.push(`## ${MODULE_TITLES[id]}`, "");
    const intro = prose(module.doc, lang.fence);
    if (intro) body.push(intro, "");
    const block = importBlock(language, id, module, items);
    if (block) body.push(code(lang.fence, block), "");
    if (module.namespace) {
      const top = items.filter(isTopLevel).map((i) => `\`${i.name}\``);
      body.push(
        language === "python"
          ? `These are used as \`${id}.<name>\`.${top.length ? ` Also exported at the top level: ${top.join(", ")}.` : ""}`
          : `These are used as \`${id}.<name>\`.${top.length ? ` Also exported at the top level: ${top.join(", ")}.` : ""}`,
        "",
      );
    }
    for (const item of items) body.push(renderItem(language, item));
  }
  const other = language === "python" ? "typescript" : "python";
  const crate = "agent";
  const head = [
    "---",
    `title: "${lang.label} API: ${page.title}"`,
    `description: "${lang.label} API reference for ${page.title.toLowerCase()}: ${names.slice(0, 8).join(", ")}${names.length > 8 ? ", and more" : ""}."`,
    "---",
    "",
    `Generated from the source of \`${lang.package}\` ${data.version} (${language === "python" ? "docstrings and type hints" : "declarations and doc comments"}) every time the site is built. Also in [${LANGUAGES[other].label}](/docs/reference/api/${other}/${page.slug}/) and [Rust](/docs/reference/api/rust/). Back to the [API reference](/docs/reference/api/).`,
    "",
  ];
  void crate;
  return head.concat(body).join("\n").replace(/\n{3,}/g, "\n\n").trimEnd() + "\n";
}

// ---------- the Rust page and the sidebar ----------

function renderRustPage(crates) {
  const rows = crates.map((c) => `| [\`${c.name}\`](/rustdoc/${c.module}/) | ${c.description.replace(/\|/g, "\\|")} |`);
  return [
    "---",
    'title: "Rust API"',
    'description: "Rust API reference: every public item of the Criogen crates, with documentation, signatures and examples, from rustdoc."',
    "---",
    "",
    "The Rust reference is rustdoc's own: every public item of the crates below, with its documentation, signatures and examples, built from the doc comments every time the site is built. Back to the [API reference](/docs/reference/api/).",
    "",
    "| Crate | What it is |",
    "|---|---|",
    ...rows,
    "",
    "Start with `criogen_agent` (agents and tools) and `criogen_core` (the workflow engine). The crates are not on crates.io yet, so these pages are the reference for their public API; the source is not published. Use the search box on any of these pages to find a type or function.",
    "",
    "Python and TypeScript have the same features: [Python API](/docs/reference/api/python/agents/), [TypeScript API](/docs/reference/api/typescript/agents/).",
    "",
  ].join("\n");
}

function renderNav() {
  const group = (label, language) => ({
    label: `${label} API`,
    items: PAGES.map((p) => ({ label: p.title, slug: `reference/api/${language}/${p.slug}` })),
  });
  const groups = [group("Python", "python"), group("TypeScript", "typescript"), { label: "Rust API", items: [{ label: "Crates", slug: "reference/api/rust" }] }];
  return (
    "// Generated by scripts/gen-api.mjs (npm run build): do not edit.\n" +
    'import type { NavGroup } from "./nav";\n\n' +
    `export const API_NAV: NavGroup[] = ${JSON.stringify(groups, null, 2)};\n`
  );
}

// ---------- run ----------

function extractPython() {
  const script = path.join(root, "scripts", "api", "python.py");
  for (const exe of ["python", "python3", "py"]) {
    const run = spawnSync(exe, ["-I", script], { encoding: "utf8", maxBuffer: 64 * 1024 * 1024 });
    if (run.status === 0) return JSON.parse(run.stdout);
    if (run.error?.code !== "ENOENT") throw new Error(`python extraction failed:\n${run.stderr}`);
  }
  throw new Error("Python is needed to read the Python API (python, python3 or py on the PATH)");
}

function checkCoverage(language, data) {
  const mapped = new Set(PAGES.flatMap((p) => p.modules));
  const lost = Object.keys(data.modules).filter((id) => !mapped.has(id) && shownItems(data.modules[id], id).length);
  if (lost.length) {
    throw new Error(`${language}: modules with no page in scripts/gen-api.mjs (PAGES): ${lost.join(", ")}`);
  }
}

function generate() {
  const files = new Map();
  const data = { python: extractPython(), typescript: extractTypescript(root) };
  for (const [language, extracted] of Object.entries(data)) {
    checkCoverage(language, extracted);
    for (const page of PAGES) {
      files.set(path.join("content", "docs", "reference", "api", language, `${page.slug}.mdx`), renderPage(language, page, extracted));
    }
  }
  files.set(path.join("content", "docs", "reference", "api", "rust.mdx"), renderRustPage(rustCrates(path.resolve(root, ".."))));
  files.set(path.join("lib", "api-nav.ts"), renderNav());
  return files;
}

if (args.includes("--rust")) {
  buildRustDocs(root);
} else {
  const files = generate();
  if (args.includes("--check")) {
    const stale = [];
    for (const [file, text] of files) {
      const full = path.join(root, file);
      const have = fs.existsSync(full) ? fs.readFileSync(full, "utf8").replace(/\r\n/g, "\n") : null;
      if (have !== text) stale.push(file);
    }
    if (stale.length) {
      console.error(`The API reference is out of date (run \`node scripts/gen-api.mjs\` in website/ and commit):\n  ${stale.join("\n  ")}`);
      process.exit(1);
    }
    console.log(`API reference up to date (${files.size} files)`);
  } else {
    for (const [file, text] of files) {
      const full = path.join(root, file);
      fs.mkdirSync(path.dirname(full), { recursive: true });
      if (!fs.existsSync(full) || fs.readFileSync(full, "utf8").replace(/\r\n/g, "\n") !== text) fs.writeFileSync(full, text);
    }
    console.log(`[api] wrote ${files.size} files`);
  }
}
