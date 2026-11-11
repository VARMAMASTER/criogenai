// Extracts the public TypeScript API of `criogen` from its source, with the
// TypeScript compiler's own symbol table, and returns it as plain data (the
// same shape scripts/api/python.py prints). Public means what
// sdks/typescript/lib/index.ts exports, namespaces (`patterns`, `evals`, ...)
// included. Nothing is built or run: the compiler only reads the sources.
import fs from "node:fs";
import path from "node:path";
import ts from "typescript";

const FLAGS = ts.TypeFormatFlags.NoTruncation | ts.TypeFormatFlags.UseAliasDefinedOutsideCurrentScope;

export function extractTypescript(root) {
  const sdk = path.join(root, "..", "sdks", "typescript");
  const entry = path.join(sdk, "lib", "index.ts");
  const config = ts.readConfigFile(path.join(sdk, "tsconfig.json"), ts.sys.readFile);
  const parsed = ts.parseJsonConfigFileContent(config.config, ts.sys, sdk);
  const program = ts.createProgram([entry], { ...parsed.options, noEmit: true, types: [], skipLibCheck: true });
  const checker = program.getTypeChecker();
  const source = program.getSourceFile(entry);
  const moduleSymbol = checker.getSymbolAtLocation(source);

  /** A type as written in a signature: optional things need no `| undefined`. */
  const tidy = (t) =>
    t
      .replace(/(\?: [^;{}]*?) \| undefined(?=[;}])/g, "$1")
      .replace(/(\?: [^;,(){}]*?) \| undefined(?=[,)])/g, "$1");
  const text = (type) => tidy(checker.typeToString(type, undefined, FLAGS));

  function resolve(symbol) {
    return symbol.flags & ts.SymbolFlags.Alias ? checker.getAliasedSymbol(symbol) : symbol;
  }

  const hasTag = (symbol, name) => symbol.getJsDocTags(checker).some((t) => t.name === name);

  function doc(symbol) {
    const body = ts.displayPartsToString(symbol.getDocumentationComment(checker)).trim();
    const parts = [body];
    const tags = symbol.getJsDocTags(checker);
    const label = (tag) => ts.displayPartsToString(tag.text ?? []).trim();
    const params = tags.filter((t) => t.name === "param");
    if (params.length) {
      parts.push(
        params
          .map((t) => {
            const raw = label(t);
            const [name, ...rest] = raw.split(/\s+/);
            return `- \`${name}\`: ${rest.join(" ").replace(/^-\s*/, "")}`;
          })
          .join("\n"),
      );
    }
    for (const tag of tags) {
      if (tag.name === "returns" || tag.name === "return") parts.push(`**Returns:** ${label(tag)}`);
      else if (tag.name === "deprecated") parts.push(`**Deprecated.** ${label(tag)}`.trim());
      else if (tag.name === "throws") parts.push(`**Throws:** ${label(tag)}`);
      else if (tag.name === "example") parts.push(`**Example**\n\n${fence(label(tag))}`);
    }
    return parts.filter(Boolean).join("\n\n");
  }

  /** An @example body: already code, or a fenced block. */
  function fence(code) {
    return code.includes("```") ? code : "```ts\n" + code + "\n```";
  }

  function typeParameters(declaration) {
    const list = declaration.typeParameters;
    return list?.length ? `<${list.map((p) => p.getText()).join(", ")}>` : "";
  }

  /** `(a: string, b?: number): Result`, from a signature. */
  function signatureText(signature, name) {
    const declaration = signature.getDeclaration();
    const params = signature.getParameters().map((parameter) => {
      const decl = parameter.valueDeclaration;
      const rest = decl && ts.isParameter(decl) && decl.dotDotDotToken ? "..." : "";
      const optional = decl && ts.isParameter(decl) && (decl.questionToken || decl.initializer) ? "?" : "";
      const type = decl ? checker.getTypeOfSymbolAtLocation(parameter, decl) : checker.getTypeOfSymbol(parameter);
      return `${rest}${parameter.getName()}${optional}: ${optional ? text(type).replace(/ \| undefined$/, "") : text(type)}`;
    });
    const generics = declaration ? typeParameters(declaration) : "";
    return `${name}${generics}(${params.join(", ")}): ${text(signature.getReturnType())}`;
  }

  function oneOrMany(signatures, name, prefix) {
    return signatures.map((s) => prefix + signatureText(s, name)).join("\n");
  }

  const isHidden = (symbol) => {
    const name = symbol.getName();
    if (name.startsWith("#") || name.startsWith("_") || hasTag(symbol, "internal")) return true;
    const decl = symbol.valueDeclaration ?? symbol.declarations?.[0];
    if (!decl) return false;
    const flags = ts.getCombinedModifierFlags(decl);
    return Boolean(flags & (ts.ModifierFlags.Private | ts.ModifierFlags.Protected));
  };

  function memberOf(symbol, owner, isStatic) {
    const decl = symbol.valueDeclaration ?? symbol.declarations?.[0];
    const type = decl ? checker.getTypeOfSymbolAtLocation(symbol, decl) : checker.getTypeOfSymbol(symbol);
    const calls = checker.getSignaturesOfType(type, ts.SignatureKind.Call);
    const prefix = isStatic ? "static " : "";
    const isMethod = decl && (ts.isMethodDeclaration(decl) || ts.isMethodSignature(decl));
    if (isMethod || (calls.length && !(symbol.flags & ts.SymbolFlags.Accessor) && type.getProperties().length === 0)) {
      return { name: symbol.getName(), kind: isStatic ? "static method" : "method", signature: oneOrMany(calls, symbol.getName(), prefix), doc: doc(symbol) };
    }
    const optional = symbol.flags & ts.SymbolFlags.Optional ? "?" : "";
    const readonly = decl && (ts.getCombinedModifierFlags(decl) & ts.ModifierFlags.Readonly || (ts.isGetAccessor(decl) && !symbol.declarations.some(ts.isSetAccessor))) ? "readonly " : "";
    return {
      name: symbol.getName(),
      kind: isStatic ? "static property" : "property",
      signature: `${prefix}${readonly}${symbol.getName()}${optional}: ${optional ? text(type).replace(/ \| undefined$/, "") : text(type)}`,
      doc: doc(symbol),
    };
  }

  function classItem(name, symbol, decl, access) {
    const instance = checker.getDeclaredTypeOfSymbol(symbol);
    const constructorType = checker.getTypeOfSymbolAtLocation(symbol, decl);
    const heritage = (decl.heritageClauses ?? []).map((h) => h.getText()).join(" ");
    const head = `${decl.modifiers?.some((m) => m.kind === ts.SyntaxKind.AbstractKeyword) ? "abstract " : ""}class ${name}${typeParameters(decl)}${heritage ? " " + heritage : ""}`;
    const constructors = checker.getSignaturesOfType(constructorType, ts.SignatureKind.Construct).map((s) => "  " + signatureText(s, "constructor").replace(/: [^:]*$/, ";"));
    const own = (property) => (property.declarations ?? []).some((d) => d.parent === decl);
    const members = [
      ...checker.getPropertiesOfType(constructorType).filter((p) => p.getName() !== "prototype" && own(p) && !isHidden(p)).map((p) => memberOf(p, name, true)),
      ...checker.getPropertiesOfType(instance).filter((p) => own(p) && !isHidden(p)).map((p) => memberOf(p, name, false)),
    ];
    const lines = [`${head} {`, ...constructors, "}"];
    return { name, kind: "class", signature: lines.join("\n"), doc: doc(symbol), members, access };
  }

  function itemFor(name, symbol, access) {
    const target = resolve(symbol);
    const decl = target.declarations?.[0];
    if (!decl || hasTag(target, "internal")) return null;
    if (ts.isClassDeclaration(decl)) return { decl, item: classItem(name, target, decl, access) };
    if (ts.isFunctionDeclaration(decl)) {
      const type = checker.getTypeOfSymbolAtLocation(target, decl);
      const calls = checker.getSignaturesOfType(type, ts.SignatureKind.Call);
      return { decl, item: { name, kind: "function", signature: oneOrMany(calls, name, "function ").replace(/\n/g, "\n"), doc: doc(target), members: [], access } };
    }
    if (ts.isVariableDeclaration(decl)) {
      const type = checker.getTypeOfSymbolAtLocation(target, decl);
      const calls = checker.getSignaturesOfType(type, ts.SignatureKind.Call);
      if (calls.length && type.getProperties().length === 0) {
        return { decl, item: { name, kind: "function", signature: oneOrMany(calls, name, "function "), doc: doc(target), members: [], access } };
      }
      return { decl, item: { name, kind: "constant", signature: `const ${name}: ${text(type)}`, doc: doc(target), members: [], access } };
    }
    if (ts.isInterfaceDeclaration(decl) || ts.isTypeAliasDeclaration(decl) || ts.isEnumDeclaration(decl)) {
      const kind = ts.isInterfaceDeclaration(decl) ? "interface" : ts.isEnumDeclaration(decl) ? "enum" : "type";
      const code = decl.getText().replace(/^export\s+/, "").replace(/^declare\s+/, "");
      return { decl, item: { name, kind, signature: code, doc: doc(target), members: [], access } };
    }
    return null;
  }

  const modules = {};
  const entryOf = (id, namespace) => (modules[id] ??= { namespace, doc: "", items: [] });
  const positions = new Map();

  const namespaces = [];
  for (const symbol of checker.getExportsOfModule(moduleSymbol)) {
    const name = symbol.getName();
    const target = resolve(symbol);
    const declaration = target.declarations?.[0];
    if (!declaration) continue;
    if (ts.isSourceFile(declaration) || (target.flags & ts.SymbolFlags.Module && ts.isSourceFile(declaration))) {
      // `export * as patterns from "./patterns"`: a namespace, handled after the top level.
      namespaces.push({ name, target, file: declaration.getSourceFile() });
      continue;
    }
    const built = itemFor(name, symbol, `import { ${name} } from "criogen"`);
    if (!built) continue;
    const id = path.basename(built.decl.getSourceFile().fileName, ".ts");
    entryOf(id, false).items.push(built.item);
    positions.set(built.item, built.decl.getStart());
  }

  for (const { name, target, file } of namespaces) {
    const entry = entryOf(name, true);
    entry.namespace = true;
    entry.doc = ts.displayPartsToString(target.getDocumentationComment(checker)).trim() || leadingComment(file);
    for (const member of checker.getExportsOfModule(target)) {
      if (entry.items.some((i) => i.name === member.getName())) continue;
      const built = itemFor(member.getName(), member, `${name}.${member.getName()}`);
      if (built) {
        entry.items.push(built.item);
        positions.set(built.item, built.decl.getStart());
      }
    }
  }

  for (const entry of Object.values(modules)) {
    entry.items.sort((a, b) => positions.get(a) - positions.get(b));
  }
  const pkg = JSON.parse(fs.readFileSync(path.join(sdk, "package.json"), "utf8"));
  return { language: "typescript", version: pkg.version, modules };
}

/** The `//` comment block at the top of a file, as prose. */
function leadingComment(file) {
  const lines = [];
  for (const line of file.text.split(/\r?\n/)) {
    if (!line.startsWith("//")) break;
    lines.push(line.replace(/^\/\/ ?/, ""));
  }
  return lines.join("\n").trim();
}
