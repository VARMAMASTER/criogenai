// The playground's engine bundle (public/engine/playground.js, design 0021):
// the website's TypeScript code samples, run as written on the engine in the
// browser. The page loads it when a pattern is run.
export * as samples from "../snippets/typescript/patterns.ts";
export { useModel } from "../snippets/typescript/model.ts";
export { observe } from "./runtime.js";
export {
  FakeReply,
  FunctionModel,
  Gateway,
  anthropic,
  gemini,
  isCriogenError,
  openai,
  openrouter,
  ready,
} from "./criogenai.js";
