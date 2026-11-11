#!/usr/bin/env python3
"""Extracts the public Python API of `criogen` from its source and prints it
as JSON (read by scripts/gen-api.mjs, which writes the reference pages).

Nothing is imported: the source is parsed with `ast`, so this runs without
the compiled engine, and a docs build never executes package code.

Public means what `criogen/__init__.py` re-exports (its `__all__`), plus the
public names of the submodules it exposes (`criogen.evals`, `criogen.mcp`,
...). Run with `python -I`.
"""

import ast
import json
import pathlib
import re
import sys

ROOT = pathlib.Path(__file__).resolve().parents[3]
PKG = ROOT / "sdks" / "python" / "python" / "criogen"
WIDTH = 88

_trees = {}


def tree_of(module):
    if module not in _trees:
        path = PKG / ("__init__.py" if module == "__init__" else f"{module}.py")
        # The compiled engine (`_native`) has no source: nothing to extract.
        _trees[module] = (
            ast.parse(path.read_text(encoding="utf-8"), filename=str(path))
            if path.exists()
            else ast.Module(body=[], type_ignores=[])
        )
    return _trees[module]


def top_level(module):
    """name -> (node, attribute docstring) for what a module defines."""
    found = {}
    body = tree_of(module).body
    for i, node in enumerate(body):
        doc = None
        nxt = body[i + 1] if i + 1 < len(body) else None
        if isinstance(nxt, ast.Expr) and isinstance(nxt.value, ast.Constant) and isinstance(nxt.value.value, str):
            doc = nxt.value.value
        if isinstance(node, (ast.ClassDef, ast.FunctionDef, ast.AsyncFunctionDef)):
            found.setdefault(node.name, (node, None))
        elif isinstance(node, ast.Assign):
            for target in node.targets:
                if isinstance(target, ast.Name):
                    found.setdefault(target.id, (node, doc))
        elif isinstance(node, ast.AnnAssign) and isinstance(node.target, ast.Name):
            found.setdefault(node.target.id, (node, doc))
    return found


def imports_of(module):
    """public name -> (module, source name) for `from .x import y`."""
    out = {}
    for node in tree_of(module).body:
        if isinstance(node, ast.ImportFrom) and node.level == 1 and node.module:
            for alias in node.names:
                out[alias.asname or alias.name] = (node.module, alias.name)
    return out


def resolve(module, name, depth=0):
    """Where `name` is defined, following re-exports: (module, node, doc)."""
    defs = top_level(module)
    if name in defs:
        node, doc = defs[name]
        return module, node, doc
    source = imports_of(module).get(name)
    if source and depth < 4:
        return resolve(source[0], source[1], depth + 1)
    return None


def literal_list(module, variable):
    for node in tree_of(module).body:
        if isinstance(node, ast.Assign) and any(isinstance(t, ast.Name) and t.id == variable for t in node.targets):
            try:
                return [x for x in ast.literal_eval(node.value)]
            except (ValueError, SyntaxError):
                return None
    return None


# ---------- signatures ----------


def unparse(node):
    return ast.unparse(node) if node is not None else ""


def param(arg, default):
    text = arg.arg
    if arg.annotation is not None:
        text += f": {unparse(arg.annotation)}"
        if default is not None:
            text += f" = {unparse(default)}"
    elif default is not None:
        text += f"={unparse(default)}"
    return text


def parameters(args, drop_first):
    parts = []
    positional = list(args.posonlyargs) + list(args.args)
    defaults = [None] * (len(positional) - len(args.defaults)) + list(args.defaults)
    for i, (arg, default) in enumerate(zip(positional, defaults)):
        if not (drop_first and i == 0 and arg.arg in ("self", "cls")):
            parts.append(param(arg, default))
        if args.posonlyargs and i == len(args.posonlyargs) - 1:
            parts.append("/")
    if args.vararg:
        parts.append("*" + param(args.vararg, None))
    elif args.kwonlyargs:
        parts.append("*")
    for arg, default in zip(args.kwonlyargs, args.kw_defaults):
        parts.append(param(arg, default))
    if args.kwarg:
        parts.append("**" + param(args.kwarg, None))
    return parts


def signature(node, drop_first, name=None):
    keyword = "async def" if isinstance(node, ast.AsyncFunctionDef) else "def"
    parts = parameters(node.args, drop_first)
    returns = f" -> {unparse(node.returns)}" if node.returns is not None else ""
    one = f"{keyword} {name or node.name}({', '.join(parts)}){returns}"
    if len(one) <= WIDTH or not parts:
        return one
    lines = "".join(f"    {p},\n" for p in parts)
    return f"{keyword} {name or node.name}(\n{lines}){returns}"


def decorator_names(node):
    names = []
    for d in node.decorator_list:
        target = d.func if isinstance(d, ast.Call) else d
        names.append(unparse(target))
    return names


def short(node, limit=70):
    text = unparse(node)
    return text if len(text) <= limit else "..."


# ---------- items ----------


def docstring(node):
    return ast.get_docstring(node, clean=True) or ""


def method_items(cls):
    """Public methods and properties defined in the class body."""
    grouped = {}
    order = []
    for node in cls.body:
        if isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef)) and not node.name.startswith("_"):
            if node.name not in grouped:
                order.append(node.name)
            grouped.setdefault(node.name, []).append(node)
    members = []
    for name in order:
        nodes = grouped[name]
        decorators = [decorator_names(n) for n in nodes]
        overloads = [n for n, d in zip(nodes, decorators) if "overload" in d or "typing.overload" in d]
        impl = [n for n in nodes if n not in overloads] or nodes
        main = impl[-1]
        prefix = ""
        kind = "method"
        names = decorator_names(main)
        for d in names:
            if d in ("staticmethod", "classmethod"):
                prefix = f"@{d}\n"
                kind = d
        if "property" in names or "cached_property" in names:
            kind = "property"
        # `self` and `cls` are implied; a static method has neither.
        drop = "staticmethod" not in names
        if kind == "property":
            sig = f"{name}: {unparse(main.returns)}" if main.returns is not None else name
        else:
            shown = overloads or [main]
            sig = "\n".join(prefix + signature(n, drop) for n in shown)
        members.append({"name": name, "kind": kind, "signature": sig, "doc": docstring(main)})
    return members


def class_item(name, cls, module, access):
    lines = []
    for d in cls.decorator_list:
        lines.append(f"@{unparse(d)}")
    bases = [unparse(b) for b in cls.bases] + [unparse(k.value) and f"{k.arg}={unparse(k.value)}" for k in cls.keywords]
    head = f"class {name}" + (f"({', '.join(bases)})" if bases else "") + ":"
    lines.append(head)
    fields = []
    body = cls.body
    for i, node in enumerate(body):
        if isinstance(node, ast.AnnAssign) and isinstance(node.target, ast.Name) and not node.target.id.startswith("_"):
            text = f"    {node.target.id}: {unparse(node.annotation)}"
            if node.value is not None:
                text += f" = {short(node.value)}"
            lines.append(text)
            nxt = body[i + 1] if i + 1 < len(body) else None
            if isinstance(nxt, ast.Expr) and isinstance(nxt.value, ast.Constant) and isinstance(nxt.value.value, str):
                fields.append((node.target.id, ast.get_docstring(ast.Module(body=[nxt], type_ignores=[]), clean=True) or ""))
    init = next((n for n in body if isinstance(n, (ast.FunctionDef, ast.AsyncFunctionDef)) and n.name == "__init__"), None)
    if init is not None:
        sig = signature(init, False)
        lines.extend(("    " + line) if line else line for line in sig.splitlines())
    if lines[-1] == head:
        # Nothing to show inside the class: no dangling colon.
        lines[-1] = head[:-1]
    doc = docstring(cls)
    if fields:
        doc += "\n\n" + "\n".join(f"- `{n}`: {d}" for n, d in fields if d)
    return {
        "name": name,
        "kind": "class",
        "signature": "\n".join(lines),
        "doc": doc.strip(),
        "members": method_items(cls),
        "access": access,
    }


def function_item(name, node, access):
    return {
        "name": name,
        "kind": "function",
        "signature": signature(node, False, name),
        "doc": docstring(node),
        "members": [],
        "access": access,
    }


def constant_item(name, node, attribute_doc, access):
    if isinstance(node, ast.AnnAssign):
        text = f"{name}: {unparse(node.annotation)}" + (f" = {short(node.value, 90)}" if node.value is not None else "")
    else:
        text = f"{name} = {short(node.value, 90)}"
    return {
        "name": name,
        "kind": "constant",
        "signature": text,
        "doc": (attribute_doc or "").strip(),
        "members": [],
        "access": access,
    }


def item_for(module, name, access):
    found = resolve(module, name)
    if found is None:
        return None, None
    home, node, attribute_doc = found
    if isinstance(node, ast.ClassDef):
        return home, class_item(name, node, home, access)
    if isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef)):
        return home, function_item(name, node, access)
    return home, constant_item(name, node, attribute_doc, access)


# ---------- the package ----------


def main():
    init = tree_of("__init__")
    exports = {}
    namespaces = []
    for node in init.body:
        if isinstance(node, ast.ImportFrom) and node.level == 1:
            if node.module is None:
                namespaces.extend(a.name for a in node.names)
            else:
                for alias in node.names:
                    exports[alias.asname or alias.name] = (node.module, alias.name)
    all_names = literal_list("__init__", "__all__") or []

    modules = {}

    def module_entry(name):
        if name not in modules:
            modules[name] = {
                "namespace": name in namespaces,
                "doc": docstring(tree_of(name)),
                "items": {},
            }
        return modules[name]

    # What the package exports at its top level.
    for name in all_names:
        if name not in exports:
            continue
        source_module, source_name = exports[name]
        home, item = item_for(source_module, source_name, f"from criogen import {name}")
        if item is None or home is None:
            continue
        item["name"] = name
        module_entry(home)["items"].setdefault(name, item)

    # What the submodules expose as `criogen.<module>.<name>`.
    for ns in namespaces:
        public = literal_list(ns, "__all__")
        if public is None:
            public = [
                n
                for n, (node, _) in top_level(ns).items()
                if isinstance(node, (ast.ClassDef, ast.FunctionDef, ast.AsyncFunctionDef)) and not n.startswith("_")
            ]
        for name in public:
            if name.startswith("_"):
                continue
            home, item = item_for(ns, name, f"criogen.{ns}.{name}")
            if item is None or home is None:
                continue
            module_entry(ns)["items"].setdefault(name, item)

    # Definition order within each module.
    out = {}
    for module, entry in modules.items():
        defs = list(top_level(module).keys())
        ordered = sorted(entry["items"].values(), key=lambda i: defs.index(i["name"]) if i["name"] in defs else 10**6)
        out[module] = {"namespace": entry["namespace"], "doc": entry["doc"], "items": ordered}

    version = re.search(r'^version\s*=\s*"([^"]+)"', (ROOT / "sdks" / "python" / "pyproject.toml").read_text(encoding="utf-8"), re.M)
    json.dump({"language": "python", "version": version.group(1) if version else "", "modules": out}, sys.stdout, indent=1, sort_keys=True)


if __name__ == "__main__":
    main()
