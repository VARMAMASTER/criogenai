// "criogen" inside the playground's samples bundle (scripts/build-engine.mjs,
// design 0021): the real engine in the browser (./criogen.js, served next to
// this bundle), re-exported unchanged, plus a report to the page of what the
// samples do (their tools, workflow nodes and storage), so the playground can
// tell the story as it happens. Nothing is reimplemented: every call goes to
// the engine as written.
import * as engine from "./criogen.js";

export * from "./criogen.js";

let observer = {};

/** The page's listener: `tool(name, phase, value)`, `node(name, phase)`, `storage(op, ns, key, value)`, `note(text)`. */
export function observe(next) {
  observer = next ?? {};
}

/** `tool()`, reporting each call before and after it runs. */
export function tool(definition) {
  return engine.tool({
    ...definition,
    run: async (args, ctx) => {
      await observer.tool?.(definition.name, "start", args);
      // The tool's own time, without the page's narration.
      const started = performance.now();
      try {
        const out = await definition.run(args, ctx);
        await observer.tool?.(definition.name, "end", out, started, performance.now());
        return out;
      } catch (error) {
        await observer.tool?.(definition.name, "error", error, started, performance.now());
        throw error;
      }
    },
  });
}

const watched = (name, fn) => async (state, ctx) => {
  await observer.node?.(name, "start", state);
  const out = await fn(state, ctx);
  await observer.node?.(name, "end", out);
  return out;
};

/** `Workflow`, reporting each of its own nodes as it starts and ends. */
export class Workflow extends engine.Workflow {
  node(name, fn) {
    return super.node(name, watched(name, fn));
  }

  addNode(name, node) {
    return super.addNode(name, typeof node === "function" ? watched(name, node) : node);
  }
}

/** `Storage`: in this tab's memory (a database needs a server), reporting recall and remember. */
export class Storage extends engine.Storage {
  constructor(options = {}) {
    const url = typeof options === "string" ? options : options?.url;
    if (url) observer.note?.(`Storage is in this tab's memory here: ${url} needs a server. The code is the same.`);
    super(url ? (typeof options === "string" ? {} : { ...options, url: undefined }) : options);
  }

  async recall(namespace, key) {
    const value = await super.recall(namespace, key);
    await observer.storage?.("recall", namespace, key, value);
    return value;
  }

  async remember(namespace, key, value, options) {
    await super.remember(namespace, key, value, options);
    await observer.storage?.("remember", namespace, key, value);
  }
}
