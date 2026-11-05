# criogen-js

The bridge between [Criogen](https://criogen.vercel.app)'s Rust engine and
JavaScript (design 0021), shared by the TypeScript SDK's two engines:

- the Node addon (`sdks/typescript`, napi-rs) on servers, and
- the WebAssembly module (`sdks/typescript/web`, wasm-bindgen) in the browser
  and other JavaScript hosts.

Everything crosses as JSON text; tools, hooks, nodes and routers are
JavaScript functions `(request JSON) => Promise<envelope JSON>`. This crate
reads the configurations, builds agents, workflows and the gateway, and turns
results and failures back into JSON, so both engines behave the same. The
bindings only adapt calling conventions.

Internal to Criogen: use the TypeScript package, `criogen`.
