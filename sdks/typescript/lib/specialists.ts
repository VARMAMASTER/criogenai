// Specialist agents: default recipes for common jobs that you configure,
// extend or replace (design 0019).
//
//   import { specialists } from "@agentbuild/sdk";
//   const agent = specialists.researcher.build("openai:gpt-5-mini", { tools: [search] });
//   const careful = specialists.researcher.extend("careful-researcher", { add: "Always give two sources." });
//   specialists.register(careful);
//
// A Specialist is a recipe, not an agent: a name, a description (what a
// coordinator reads to decide when to use it), instructions and default agent
// options. `build` makes the agent. The built-in ones are prompts and sensible
// defaults; tools come from you, and no claim is made about their quality on
// your data.

import { Agent, type AgentOptions } from "./agent";
import type { Model } from "./models";
import type { Tool } from "./tools";

const NAME_LIMIT = 64;

/** Agent options a recipe may carry (everything but the model). */
export type SpecialistSettings = Omit<AgentOptions, "model" | "instructions"> & {
  /** Called when `build` gets no tools of its own: e.g. web search when a key is set. */
  defaultTools?: () => Tool<any>[];
};

export interface ExtendOptions extends SpecialistSettings {
  /** Appended to the instructions. */
  add?: string;
  /** Replaces the instructions. */
  instructions?: string;
  description?: string;
}

/** A recipe for an agent. */
export class Specialist {
  readonly settings: SpecialistSettings;

  constructor(
    readonly name: string,
    readonly description: string,
    readonly instructions: string,
    settings: SpecialistSettings = {},
  ) {
    if (!/^[A-Za-z0-9_-]+$/.test(name) || name.length > NAME_LIMIT) {
      throw new TypeError(`specialist name "${name}": use letters, digits, - and _ (at most ${NAME_LIMIT})`);
    }
    if (!description.trim()) throw new TypeError("a specialist needs a description: coordinators choose by it");
    this.settings = { ...settings };
  }

  /**
   * The agent. `overrides` win over the recipe's settings; `instructions`
   * replaces the instructions and `tools` replaces the tools (use `extend` to add).
   */
  build(model: Model, overrides: SpecialistSettings & { instructions?: string } = {}): Agent<any> {
    const { instructions, defaultTools, ...settings } = { ...this.settings, ...overrides } as SpecialistSettings & { instructions?: string };
    let tools = [...(settings.tools ?? [])] as Tool<any>[];
    if (tools.length === 0 && defaultTools) tools = defaultTools(); // plug and play: web search when a key is set
    return new Agent({ ...settings, model, instructions: instructions ?? this.instructions, tools });
  }

  /**
   * A new recipe from this one: `add` appends to the instructions,
   * `instructions` replaces them, `tools` are added to the existing ones, other
   * options override settings. This recipe is unchanged.
   */
  extend(name?: string, options: ExtendOptions = {}): Specialist {
    const { add, instructions, description, tools, ...settings } = options;
    let text = instructions ?? this.instructions;
    if (add) text = `${text.trimEnd()}\n\n${add.trim()}`;
    return new Specialist(name ?? `${this.name}-extended`, description ?? this.description, text, {
      ...this.settings,
      ...settings,
      tools: [...(this.settings.tools ?? []), ...(tools ?? [])],
    });
  }

  toString(): string {
    return `Specialist(${this.name})`;
  }
}

const registry = new Map<string, Specialist>();

/** Keeps a recipe under its name, so workflows and configuration can refer to it by name. A taken name throws unless `replace`. */
export function register(specialist: Specialist, options: { replace?: boolean } = {}): Specialist {
  if (registry.has(specialist.name) && !options.replace) {
    throw new Error(`a specialist named "${specialist.name}" is already registered (replace: true to overwrite)`);
  }
  registry.set(specialist.name, specialist);
  return specialist;
}

/** Forgets a registered recipe (the built-in ones stay available as module exports). */
export function unregister(name: string): void {
  registry.delete(name);
}

/** The recipe registered under `name` (throws, listing the names). */
export function get(name: string): Specialist {
  const found = registry.get(name);
  if (!found) throw new Error(`no specialist "${name}"; registered: ${available().join(", ")}`);
  return found;
}

/** Registered names, sorted. */
export function available(): string[] {
  return [...registry.keys()].sort();
}

function builtin(name: string, description: string, instructions: string, settings: SpecialistSettings = {}): Specialist {
  return register(new Specialist(name, description, instructions, settings));
}

function webTools(): Tool<any>[] {
  // Loaded when needed: the specialists work without the web tools.
  // eslint-disable-next-line @typescript-eslint/no-require-imports
  return (require("./web") as typeof import("./web")).defaultTools();
}

export const researcher = builtin(
  "researcher",
  "Finds and gathers facts on a topic, with sources, using its tools. Use it before writing.",
  "You are a careful researcher. Use your tools to find facts; never invent facts or sources. " +
    "Reply with short bullet points, each with where it came from. If you cannot find something, say so.",
  { defaultTools: webTools }, // web search + page reading when TAVILY_API_KEY or BRAVE_API_KEY is set
);
export const summarizer = builtin(
  "summarizer",
  "Condenses long text into a short, faithful summary.",
  "You summarize. Keep every important fact and number, drop the rest, add nothing that is not in the text. Reply with the summary only.",
);
export const classifier = builtin(
  "classifier",
  "Puts a message into one category and rates its urgency. Use it to triage.",
  "You classify the user's message. Choose exactly one category from the list you were given (if none was given, " +
    "use: billing, shipping, technical, other) and rate urgency as low, medium or high. Reply as JSON: " +
    '{"category": "...", "urgency": "..."}.',
);
export const extractor = builtin(
  "extractor",
  "Pulls structured fields out of free text. Give it an output schema.",
  "You extract fields from the user's text. Fill only what the text states; leave a field null when it is not there. Never guess.",
);
export const planner = builtin(
  "planner",
  "Breaks a goal into a short ordered list of concrete steps.",
  "You plan. Break the goal into at most 7 concrete, ordered steps, each one a single action someone could do. Reply with a numbered list only.",
);
export const writer = builtin(
  "writer",
  "Writes clear prose from notes or a brief. Use it after research.",
  "You are a writer. Turn the notes you are given into clear, well-organised prose for the stated audience. " +
    "Use only facts from the notes; if the notes are thin, say what is missing instead of filling it in.",
);
export const critic = builtin(
  "critic",
  "Reviews a draft against criteria and either approves it or says what to fix.",
  "You are a strict reviewer. Check the draft against the criteria you were given (accuracy, completeness, clarity, " +
    "and anything the user specified). If it is good enough to use, reply with exactly: APPROVED. Otherwise reply with: " +
    "REVISE: followed by a short, specific list of what to change.",
);
export const factChecker = builtin(
  "fact_checker",
  "Checks claims in a text against its tools and reports which hold.",
  "You check facts. List each factual claim in the text, verify it with your tools, and label it SUPPORTED, " +
    "CONTRADICTED or UNVERIFIED with one line of evidence. Never mark a claim SUPPORTED without evidence.",
);
export const codeReviewer = builtin(
  "code_reviewer",
  "Reviews code for bugs, security problems and unclear parts.",
  "You review code. Report real bugs first, then security problems, then readability, each with the line or " +
    "function and a concrete fix. If the code is fine, reply with exactly: APPROVED.",
);
export const translator = builtin(
  "translator",
  "Translates text faithfully into the language it is asked for.",
  "You translate. Keep the meaning, tone and formatting; do not add or remove information. Reply with the translation only.",
);
export const supportAgent = builtin(
  "support_agent",
  "Answers customer questions politely and briefly, using its tools for facts.",
  "You are a friendly customer support agent. Be brief. Use your tools for facts about orders and accounts; never " +
    "guess. If an action needs a person's approval, say it is waiting for approval.",
);
