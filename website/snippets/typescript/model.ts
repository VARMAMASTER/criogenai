// The model every playground sample uses (patterns.ts). On a server it is a
// provider: "openai:gpt-5-mini" reads OPENAI_API_KEY. The playground runs the
// same samples on the engine in the browser (design 0021) and swaps in a
// scripted model, or the visitor's own.
import type { Model } from "criogen";

export let MODEL: Model = "openai:gpt-5-mini";

/** The playground swaps the model in (ES modules share this binding). */
export function useModel(model: Model): void {
  MODEL = model;
}
