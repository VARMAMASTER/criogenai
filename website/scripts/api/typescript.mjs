// Extracts the public TypeScript API of `criogenai` from its source, with the
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

  const label = (tag) => ts.displayPartsToString(tag.text ?? []).trim();

  /** The description: the comment's body, and a deprecation notice. */
  function doc(symbol) {
    const parts = [ts.displayPartsToString(symbol.getDocumentationComment(checker)).trim()];
    for (const tag of symbol.getJsDocTags(checker)) {
      if (tag.name === "deprecated") parts.push(`**Deprecated.** ${label(tag)}`.trim());
    }
    return parts.filter(Boolean).join("\n\n");
  }

  /** returns, raises and examples, from @returns, @throws and @example. */
  function tagged(symbol) {
    return taggedFrom(symbol.getJsDocTags(checker));
  }

  function taggedFrom(tags) {
    const out = { returns: "", raises: [], examples: [] };
    for (const tag of tags) {
      if (tag.name === "returns" || tag.name === "return") out.returns = label(tag);
      else if (tag.name === "throws") {
        const text = label(tag);
        const named = /^\{?([A-Za-z_][\w.]*)\}?\s+(?:-\s*)?([\s\S]*)$/.exec(text);
        out.raises.push(named && /Error$/.test(named[1]) ? { name: named[1], doc: named[2] } : { name: "", doc: text });
      } else if (tag.name === "example") {
        out.examples.push(label(tag).replace(/^```\w*\n?/, "").replace(/\n?```\s*$/, ""));
      }
    }
    return out;
  }

  /** [{name, type, default, doc}] for a signature's parameters, with their @param text. */
  function paramsOf(signature) {
    return signature.getParameters().map((parameter) => {
      const decl = parameter.valueDeclaration;
      const isParam = decl && ts.isParameter(decl);
      const optional = isParam && (decl.questionToken || decl.initializer);
      const type = decl ? checker.getTypeOfSymbolAtLocation(parameter, decl) : checker.getTypeOfSymbol(parameter);
      let name = parameter.getName();
      if (isParam && decl.dotDotDotToken) name = "..." + name;
      if (isParam && ts.isObjectBindingPattern(decl.name)) name = "options";
      return {
        name,
        type: optional ? text(type).replace(/ \| undefined$/, "") : text(type),
        default: isParam && decl.initializer ? decl.initializer.getText() : optional ? "" : null,
        doc: ts.displayPartsToString(parameter.getDocumentationComment(checker)).trim(),
      };
    });
  }

  /** The documented properties of an interface or object type: [{name, type, default, doc}]. */
  function fieldsOf(symbol, decl) {
    const isObject = ts.isInterfaceDeclaration(decl) || (ts.isTypeAliasDeclaration(decl) && ts.isTypeLiteralNode(decl.type));
    if (!isObject) return [];
    const type = checker.getDeclaredTypeOfSymbol(symbol);
    return checker
      .getPropertiesOfType(type)
      .filter((p) => !isHidden(p))
      .map((p) => {
        const d = p.valueDeclaration ?? p.declarations?.[0];
        const t = d ? checker.getTypeOfSymbolAtLocation(p, d) : checker.getTypeOfSymbol(p);
        const optional = p.flags & ts.SymbolFlags.Optional;
        const defaultTag = p.getJsDocTags(checker).find((tag) => tag.name === "default" || tag.name === "defaultValue");
        return {
          name: p.getName(),
          type: optional ? text(t).replace(/ \| undefined$/, "") : text(t),
          default: defaultTag ? label(defaultTag) : optional ? "" : null,
          doc: doc(p),
        };
      });
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
      return {
        name: symbol.getName(),
        kind: isStatic ? "static method" : "method",
        signature: oneOrMany(calls, symbol.getName(), prefix),
        doc: doc(symbol),
        params: calls.length ? paramsOf(calls[calls.length - 1]) : [],
        ...tagged(symbol),
      };
    }
    const optional = symbol.flags & ts.SymbolFlags.Optional ? "?" : "";
    const readonly = decl && (ts.getCombinedModifierFlags(decl) & ts.ModifierFlags.Readonly || (ts.isGetAccessor(decl) && !symbol.declarations.some(ts.isSetAccessor))) ? "readonly " : "";
    return {
      name: symbol.getName(),
      kind: isStatic ? "static property" : "property",
      signature: `${prefix}${readonly}${symbol.getName()}${optional}: ${optional ? text(type).replace(/ \| undefined$/, "") : text(type)}`,
      doc: doc(symbol),
      params: [],
      ...tagged(symbol),
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
    const ctors = checker.getSignaturesOfType(constructorType, ts.SignatureKind.Construct);
    // The constructor's own JSDoc (its signature's tags), not the class's.
    const ctorDecl = decl.members.find(ts.isConstructorDeclaration);
    const ctorTags = ctorDecl ? taggedFrom(ts.getJSDocTags(ctorDecl).map((tag) => ({ name: tag.tagName.text, text: [{ kind: "text", text: (tag.typeExpression ? `{${tag.typeExpression.type.getText()}} ` : "") + (ts.getTextOfJSDocComment(tag.comment) ?? "") }] }))) : { returns: "", raises: [], examples: [] };
    const classTags = tagged(symbol);
    return {
      name,
      kind: "class",
      signature: lines.join("\n"),
      doc: doc(symbol),
      params: ctors.length ? paramsOf(ctors[ctors.length - 1]) : [],
      returns: "",
      raises: [...classTags.raises, ...ctorTags.raises],
      examples: [...classTags.examples, ...ctorTags.examples],
      fields: [],
      members,
      access,
    };
  }

  function itemFor(name, symbol, access) {
    const target = resolve(symbol);
    const decl = target.declarations?.[0];
    if (!decl || hasTag(target, "internal")) return null;
    if (ts.isClassDeclaration(decl)) return { decl, item: classItem(name, target, decl, access) };
    const empty = { params: [], fields: [], returns: "", raises: [], examples: [] };
    const fn = (calls) => ({
      kind: "function",
      signature: oneOrMany(calls, name, "function "),
      doc: doc(target),
      ...empty,
      params: calls.length ? paramsOf(calls[calls.length - 1]) : [],
      ...tagged(target),
      members: [],
      access,
    });
    if (ts.isFunctionDeclaration(decl)) {
      const type = checker.getTypeOfSymbolAtLocation(target, decl);
      return { decl, item: { name, ...fn(checker.getSignaturesOfType(type, ts.SignatureKind.Call)) } };
    }
    if (ts.isVariableDeclaration(decl)) {
      const type = checker.getTypeOfSymbolAtLocation(target, decl);
      const calls = checker.getSignaturesOfType(type, ts.SignatureKind.Call);
      if (calls.length && type.getProperties().length === 0) {
        return { decl, item: { name, ...fn(calls) } };
      }
      return { decl, item: { name, kind: "constant", signature: `const ${name}: ${text(type)}`, doc: doc(target), ...empty, ...tagged(target), members: [], access } };
    }
    if (ts.isInterfaceDeclaration(decl) || ts.isTypeAliasDeclaration(decl) || ts.isEnumDeclaration(decl)) {
      const kind = ts.isInterfaceDeclaration(decl) ? "interface" : ts.isEnumDeclaration(decl) ? "enum" : "type";
      const fields = fieldsOf(target, decl);
      // Doc comments are listed under the code (fields), not inside it.
      const code = stripComments(decl.getText().replace(/^export\s+/, "").replace(/^declare\s+/, ""));
      return { decl, item: { name, kind, signature: code, doc: doc(target), ...empty, ...tagged(target), fields, members: [], access } };
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
    const built = itemFor(name, symbol, `import { ${name} } from "criogenai"`);
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

/** A declaration's text without its `/** … *\/` and `//` comments (they are shown as prose). */
function stripComments(code) {
  return code
    .replace(/^[ \t]*\/\*\*[\s\S]*?\*\/[ \t]*\r?\n/gm, "")
    .replace(/[ \t]*\/\*\*[\s\S]*?\*\//g, "")
    .replace(/^[ \t]*\/\/.*\r?\n/gm, "")
    .replace(/\n{3,}/g, "\n\n");
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
