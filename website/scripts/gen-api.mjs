// The API reference for Python, TypeScript and Rust.
//
//   node scripts/gen-api.mjs           writes the Python and TypeScript pages
//                                      (content/docs/reference/api/<language>/…:
//                                      an overview and one page per module), the
//                                      Rust overview page, lib/api-nav.ts and the
//                                      list of undocumented API
//   node scripts/gen-api.mjs --check   fails if those files are not what the
//                                      sources produce now, or if public API was
//                                      added without documentation (used by CI)
//   node scripts/gen-api.mjs --rust    builds rustdoc into public/rustdoc
//
// Python is read from its source with `ast` (scripts/api/python.py), TypeScript
// with the compiler's symbol table (scripts/api/typescript.mjs), Rust with
// rustdoc (scripts/api/rust.mjs). Nothing is imported or run from the SDKs.
//
// How to document the SDKs so it shows here:
//   Python: Google style docstrings: a summary line, prose, then `Args:`,
//           `Returns:`, `Raises:` and `Example:` sections; dataclass fields
//           take a docstring on the line after them (or a `Fields:` section).
//   TypeScript: JSDoc: prose, `@param name text`, `@returns`, `@throws
//           {ErrorClass} text`, `@example` (code), and a doc comment on each
//           property of an options interface (`@default` for its default).
import { spawnSync } from "node:child_process";
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { buildRustDocs, rustCrates } from "./api/rust.mjs";
import { extractTypescript } from "./api/typescript.mjs";

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const args = process.argv.slice(2);
const UNDOCUMENTED = path.join("scripts", "api", "undocumented.json");

// ---------- what goes where ----------

/** The module pages, grouped as the overview and the sidebar show them. */
const GROUPS = [
  { label: "Agents", modules: ["agent", "tools", "hooks", "errors", "usage"] },
  { label: "Models", modules: ["models", "gateway", "env", "redact"] },
  { label: "Data", modules: ["storage", "collections"] },
  { label: "Orchestration", modules: ["workflow", "patterns", "specialists"] },
  { label: "Integrations", modules: ["mcp", "web"] },
  { label: "Quality and operations", modules: ["telemetry", "trace", "evals", "ready"] },
];

/** module id -> [page title, what the module is for (one line)] */
const MODULES = {
  agent: ["Agents", "The agent: a model, tools and instructions; runs, threads, streams and approvals."],
  tools: ["Tools", "Functions the model can call: definitions, input schemas, errors and the tool context."],
  hooks: ["Hooks", "Your code around the agent loop: before and after model and tool calls."],
  errors: ["Errors", "Every error class, one per stable error code."],
  usage: ["Usage and cost", "Tokens and an estimated price for a run."],
  models: ["Models", "Providers and models: OpenAI, Anthropic, Gemini, Azure, Bedrock, Vertex AI, local servers and more."],
  gateway: ["Gateway", "Chat, stream and embed without an agent, with retries and fallbacks."],
  env: ["Keys and environment", "Loading API keys from a file or the environment."],
  redact: ["Redaction", "Removing personal data before anything reaches a provider."],
  storage: ["Storage and memory", "Durable threads (SQLite, Postgres, in memory), encryption, audit, retention and long-term memory."],
  collections: ["Collections", "Documents to search by meaning, and a retrieval tool for agents."],
  workflow: ["Workflows", "Graphs of steps and agents: routes, joins, loops, pauses for people, and threads."],
  patterns: ["Patterns", "Ready-made multi-agent designs: sequential, parallel, loops, routers, debates and more."],
  specialists: ["Specialist agents", "Default agent recipes (researcher, writer, critic, ...) and a registry for your own."],
  mcp: ["MCP servers", "Tools from Model Context Protocol servers, over HTTP or a local process."],
  web: ["Web search and page reading", "Web search (Tavily, Brave) and safe page reading tools."],
  telemetry: ["Telemetry", "Exporting runs to Langfuse or any OpenTelemetry backend."],
  trace: ["Trace viewer", "A local page that shows your runs, with no account."],
  evals: ["Evals", "Datasets, scorers, judges, comparisons and quality gates."],
  ready: ["Readiness check", "Checks that the engine, keys and providers work before you ship."],
};

/** Names that lead their module's page (the entry points), in this order; the rest follow in source order. */
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
  python: { label: "Python", fence: "python", package: "criogen", install: "pip install criogen", source: "docstrings and type hints" },
  typescript: { label: "TypeScript", fence: "ts", package: "criogen", install: "npm install criogen", source: "declarations and doc comments" },
};

const pageHref = (language, id) => `/docs/reference/api/${language}/${id}/`;

// ---------- prose: documentation text as MDX ----------

/** Docstrings and JSDoc are Markdown-ish text; MDX also reads `{`, `<`, `import` and indented code. */
function prose(raw, fence) {
  let text = String(raw ?? "").replace(/\r\n/g, "\n").trim();
  if (!text) return "";
  text = noDesign(text);
  // reST, as written in the Python docstrings: ":meth:`run`", and double backticks (which may wrap lines).
  text = text.replace(/:[a-z]+:`([^`\n]+)`/g, "`$1`");
  text = text.replace(/``([^`]{1,200}?)``/g, (_, inner) => "`" + inner.replace(/\s*\n\s*/g, " ") + "`");
  // JSDoc {@link Name} -> `Name`.
  text = text.replace(/\{@link(?:code|plain)?\s+([^}|\s]+)(?:[\s|][^}]*)?\}/g, "`$1`");
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

/** Inline code that survives any content (a type with backticks in it gets a longer fence). */
const inline = (text) => {
  const t = String(text).replace(/\s*\n\s*/g, " ").trim();
  return t.includes("`") ? "`` " + t + " ``" : "`" + t + "`";
};

/** The first sentence of a description, as plain one-line text for a table cell. */
function summary(doc) {
  const first = noDesign(String(doc ?? "").replace(/\r\n/g, "\n").trim()).split(/\n\s*\n/)[0].replace(/\s*\n\s*/g, " ");
  if (!first || first.startsWith("```")) return "";
  // Up to the first full stop that ends a sentence (not inside `code`).
  let out = "";
  let inCode = false;
  for (let i = 0; i < first.length; i++) {
    const c = first[i];
    if (c === "`") inCode = !inCode;
    out += c;
    if (!inCode && c === "." && (i === first.length - 1 || first[i + 1] === " ") && !/\b(e\.g|i\.e|etc|vs)\.$/.test(out)) break;
  }
  return escapeMdx(out.replace(/:[a-z]+:`([^`]+)`/g, "`$1`").replace(/``([^`]+)``/g, "`$1`")).replace(/\|/g, "\\|");
}

/** The site's heading ids (lib/docs.ts `slugger`), so tables can link to headings. */
function slugger() {
  const seen = new Map();
  return (text) => {
    const base = text
      .toLowerCase()
      .trim()
      .replace(/[^\p{L}\p{M}\p{N}\p{Pc}\- ]/gu, "")
      .replace(/ /g, "-");
    const n = seen.get(base) ?? 0;
    seen.set(base, n + 1);
    return n ? `${base}-${n}` : base;
  };
}

// ---------- one item ----------

/** `- name (type, default x): text` lines for parameters or fields. */
function paramList(entries, fence) {
  return entries
    .map((p) => {
      const meta = [];
      if (p.type) meta.push(inline(p.type));
      if (p.default) meta.push(`default ${inline(p.default)}`);
      else if (p.default === "") meta.push("optional");
      const head = `- ${inline(p.name)}${meta.length ? ` (${meta.join(", ")})` : ""}`;
      const text = prose(p.doc, fence);
      if (!text) return head;
      // Continuation lines are indented under the list item.
      return `${head}: ${text.replace(/\n/g, "\n  ")}`;
    })
    .join("\n");
}

/** Description, parameters, fields, returns, raises and examples: the body under a heading. */
function details(language, entry) {
  const fence = LANGUAGES[language].fence;
  const out = [];
  const doc = prose(entry.doc, fence);
  if (doc) out.push(doc);
  const params = entry.params ?? [];
  if (params.length && params.some((p) => p.doc)) out.push("**Parameters**", paramList(params, fence));
  const fields = entry.fields ?? [];
  if (fields.length && fields.some((f) => f.doc)) out.push(entry.kind === "class" ? "**Attributes**" : "**Fields**", paramList(fields, fence));
  if (entry.returns) out.push(`**Returns:** ${prose(entry.returns, fence)}`);
  if (entry.raises?.length) {
    out.push("**Raises**", entry.raises.map((r) => `- ${r.name ? inline(r.name) + ": " : ""}${prose(r.doc, fence).replace(/\n/g, "\n  ")}`).join("\n"));
  }
  for (const [i, example] of (entry.examples ?? []).entries()) {
    out.push(i === 0 ? "**Example**" : "**Another example**", code(fence, example.replace(/^>>> |^\.\.\. /gm, "")));
  }
  return out.join("\n\n");
}

const KIND_LABEL = {
  class: "class",
  function: "function",
  constant: "constant",
  interface: "interface",
  type: "type",
  enum: "enum",
};

// ---------- the pages ----------

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

/** The modules of a language that have something to show, in GROUPS order. */
function shownModules(data) {
  const out = [];
  for (const group of GROUPS) {
    for (const id of group.modules) {
      const module = data.modules[id];
      if (module && shownItems(module, id).length) out.push({ id, group: group.label, module });
    }
  }
  return out;
}

function renderModulePage(language, id, module, data, otherData) {
  const lang = LANGUAGES[language];
  const other = language === "python" ? "typescript" : "python";
  const [title, purpose] = MODULES[id];
  const items = shownItems(module, id);
  const slug = slugger();
  const body = [];

  // Headings in page order, so the contents table links to the ids the site gives them.
  const intro = prose(module.doc, lang.fence);
  const block = importBlock(language, id, module, items);
  const ids = new Map();
  slug("Contents");
  for (const item of items) {
    ids.set(item, slug(item.name));
    for (const member of item.members) slug(`${item.name}.${member.name}`);
  }

  const crossLinks = [`[${lang.label} API overview](/docs/reference/api/${language}/)`];
  if (otherData.modules[id] && shownItems(otherData.modules[id], id).length) {
    crossLinks.push(`the same module in [${LANGUAGES[other].label}](${pageHref(other, id)})`);
  }
  body.push(`${purpose} Generated from the ${lang.source} of \`${lang.package}\` ${data.version}. See also: ${crossLinks.join(", ")}.`, "");
  if (intro) body.push(intro, "");
  if (block) body.push(code(lang.fence, block), "");
  if (module.namespace) {
    const top = items.filter(isTopLevel).map((i) => inline(i.name));
    body.push(`These are used as \`${id}.<name>\`.${top.length ? ` Also exported at the top level: ${top.join(", ")}.` : ""}`, "");
  }

  body.push("## Contents", "", "| Name | Kind | Summary |", "|---|---|---|");
  for (const item of items) {
    body.push(`| [\`${item.name}\`](#${ids.get(item)}) | ${KIND_LABEL[item.kind] ?? item.kind} | ${summary(item.doc)} |`);
  }
  body.push("");

  for (const item of items) {
    body.push(`## ${item.name}`, "", code(lang.fence, item.signature), "");
    const text = details(language, item);
    if (text) body.push(text, "");
    for (const member of item.members) {
      body.push(`### ${item.name}.${member.name}`, "", code(lang.fence, member.signature), "");
      const m = details(language, member);
      if (m) body.push(m, "");
    }
  }

  const names = items.map((i) => i.name);
  const head = [
    "---",
    `title: "${lang.label} API: ${title}"`,
    `description: "${lang.label} API reference, ${title.toLowerCase()}: ${names.slice(0, 8).join(", ")}${names.length > 8 ? ", and more" : ""}."`,
    "---",
    "",
  ];
  return head.concat(body).join("\n").replace(/\n{3,}/g, "\n\n").trimEnd() + "\n";
}

function renderLanguagePage(language, data) {
  const lang = LANGUAGES[language];
  const other = language === "python" ? "typescript" : "python";
  const shown = shownModules(data);
  const body = [
    "---",
    `title: "${lang.label} API"`,
    `description: "The ${lang.label} API reference of Criogen, one page per module: agents, tools, models, storage, workflows, patterns, MCP, telemetry and evals."`,
    "---",
    "",
    `Every public class, function and type of the \`${lang.package}\` ${lang.label} package (${data.version}), one page per module, generated from its ${lang.source} every time the site is built. Install it with \`${lang.install}\`. The same API in [${LANGUAGES[other].label}](/docs/reference/api/${other}/) and [Rust](/docs/reference/api/rust/).`,
    "",
    "New to Criogen? Start with the [quickstart](/docs/quickstart/) and the [agents guide](/docs/guides/agents/); come here for the exact names, parameters and defaults.",
    "",
  ];
  let group = null;
  for (const { id, group: label, module } of shown) {
    if (label !== group) {
      group = label;
      body.push(`## ${label}`, "", "| Page | What it covers | Main names |", "|---|---|---|");
    }
    const [title, purpose] = MODULES[id];
    const lead = shownItems(module, id)
      .slice(0, 4)
      .map((i) => `[\`${i.name}\`](${pageHref(language, id)}#${slugger()(i.name)})`)
      .join(", ");
    body.push(`| [${title}](${pageHref(language, id)}) | ${purpose} | ${lead} |`);
    const next = shown[shown.indexOf(shown.find((s) => s.id === id)) + 1];
    if (!next || next.group !== label) body.push("");
  }
  return body.join("\n").replace(/\n{3,}/g, "\n\n").trimEnd() + "\n";
}

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
    "Python and TypeScript have the same features: [Python API](/docs/reference/api/python/), [TypeScript API](/docs/reference/api/typescript/).",
    "",
  ].join("\n");
}

function renderNav(data) {
  const group = (language) => ({
    label: `${LANGUAGES[language].label} API`,
    items: [
      { label: "Overview", slug: `reference/api/${language}` },
      ...shownModules(data[language]).map(({ id }) => ({ label: MODULES[id][0], slug: `reference/api/${language}/${id}` })),
    ],
  });
  const groups = [group("python"), group("typescript"), { label: "Rust API", items: [{ label: "Crates", slug: "reference/api/rust" }] }];
  return (
    "// Generated by scripts/gen-api.mjs (npm run build): do not edit.\n" +
    'import type { NavGroup } from "./nav";\n\n' +
    `export const API_NAV: NavGroup[] = ${JSON.stringify(groups, null, 2)};\n`
  );
}

// ---------- documentation coverage ----------

/** Public things with no documentation: items, their members, parameters and fields. */
function undocumented(language, data) {
  const out = [];
  for (const { id, module } of shownModules(data)) {
    for (const item of shownItems(module, id)) {
      const name = `${language}:${id}.${item.name}`;
      if (!item.doc?.trim()) out.push(name);
      for (const p of item.params ?? []) if (!p.doc?.trim() && !/^\*|^\.\.\./.test(p.name)) out.push(`${name}(${p.name})`);
      for (const f of item.fields ?? []) if (!f.doc?.trim()) out.push(`${name}.${f.name}`);
      for (const m of item.members) {
        if (!m.doc?.trim()) out.push(`${name}.${m.name}`);
        for (const p of m.params ?? []) if (!p.doc?.trim() && !/^\*|^\.\.\./.test(p.name)) out.push(`${name}.${m.name}(${p.name})`);
      }
    }
  }
  return out;
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

function checkMapping(language, data) {
  const mapped = new Set(GROUPS.flatMap((g) => g.modules));
  const lost = Object.keys(data.modules).filter((id) => !mapped.has(id) && shownItems(data.modules[id], id).length);
  if (lost.length) {
    throw new Error(`${language}: modules with no page in scripts/gen-api.mjs (GROUPS, MODULES): ${lost.join(", ")}`);
  }
}

function generate() {
  const files = new Map();
  const data = { python: extractPython(), typescript: extractTypescript(root) };
  const missing = [];
  for (const [language, extracted] of Object.entries(data)) {
    checkMapping(language, extracted);
    const other = data[language === "python" ? "typescript" : "python"];
    const dir = path.join("content", "docs", "reference", "api", language);
    files.set(path.join(dir, "index.mdx"), renderLanguagePage(language, extracted));
    for (const { id, module } of shownModules(extracted)) {
      files.set(path.join(dir, `${id}.mdx`), renderModulePage(language, id, module, extracted, other));
    }
    missing.push(...undocumented(language, extracted));
  }
  files.set(path.join("content", "docs", "reference", "api", "rust.mdx"), renderRustPage(rustCrates(path.resolve(root, ".."))));
  files.set(path.join("lib", "api-nav.ts"), renderNav(data));
  return { files, missing: missing.sort() };
}

/** Generated pages in the API folders that the sources no longer produce. */
function strays(files) {
  const out = [];
  for (const language of Object.keys(LANGUAGES)) {
    const dir = path.join(root, "content", "docs", "reference", "api", language);
    if (!fs.existsSync(dir)) continue;
    for (const name of fs.readdirSync(dir)) {
      const rel = path.join("content", "docs", "reference", "api", language, name);
      if (!files.has(rel)) out.push(rel);
    }
  }
  return out;
}

if (args.includes("--rust")) {
  buildRustDocs(root);
} else {
  const { files, missing } = generate();
  const baselineFile = path.join(root, UNDOCUMENTED);
  const baseline = fs.existsSync(baselineFile) ? JSON.parse(fs.readFileSync(baselineFile, "utf8")) : [];
  if (args.includes("--check")) {
    const stale = [];
    for (const [file, text] of files) {
      const full = path.join(root, file);
      const have = fs.existsSync(full) ? fs.readFileSync(full, "utf8").replace(/\r\n/g, "\n") : null;
      if (have !== text) stale.push(file);
    }
    stale.push(...strays(files));
    const known = new Set(baseline);
    const added = missing.filter((m) => !known.has(m));
    if (stale.length) {
      console.error(`The API reference is out of date (run \`node scripts/gen-api.mjs\` in website/ and commit):\n  ${stale.join("\n  ")}`);
    }
    if (added.length) {
      console.error(
        `Public API without documentation (document it in the SDK source; see the top of scripts/gen-api.mjs):\n  ${added.slice(0, 40).join("\n  ")}${added.length > 40 ? `\n  … and ${added.length - 40} more` : ""}`,
      );
    }
    if (missing.length < baseline.length) {
      console.error(`${baseline.length - missing.length} undocumented entries are now documented: run \`node scripts/gen-api.mjs\` to update ${UNDOCUMENTED}.`);
    }
    if (stale.length || added.length || missing.length < baseline.length) process.exit(1);
    console.log(`API reference up to date (${files.size} files; ${missing.length} undocumented entries left)`);
  } else {
    for (const [file, text] of files) {
      const full = path.join(root, file);
      fs.mkdirSync(path.dirname(full), { recursive: true });
      if (!fs.existsSync(full) || fs.readFileSync(full, "utf8").replace(/\r\n/g, "\n") !== text) fs.writeFileSync(full, text);
    }
    for (const stray of strays(files)) fs.rmSync(path.join(root, stray));
    fs.writeFileSync(baselineFile, JSON.stringify(missing, null, 1) + "\n");
    console.log(`[api] wrote ${files.size} files; ${missing.length} undocumented entries (${UNDOCUMENTED})`);
  }
}
