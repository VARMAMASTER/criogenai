// The landing page's animated scenes: what Criogen does, as runs you can
// watch. Each one shows behaviour the framework has today (see the guides
// linked next to them); the names are examples.
import type { Beat, SceneEdge, SceneNode } from "./scene-types";

export interface SceneData {
  id: string;
  title: string;
  description: string;
  width: number;
  height: number;
  nodes: SceneNode[];
  edges: SceneEdge[];
  beats: Beat[];
}

const MAIN = 155;

export const workflow: SceneData = {
  id: "scene-workflow",
  title: "A workflow",
  description:
    "A workflow: classify a request, run three searches in parallel while a router skips the escalate branch, merge, wait for a person to approve the refund, then reply.",
  width: 900,
  height: 300,
  nodes: [
    { id: "request", label: "request", x: 70, y: MAIN, kind: "client" },
    { id: "classify", label: "classify", x: 215, y: MAIN, kind: "model" },
    { id: "escalate", label: "escalate", x: 405, y: 38, final: "skip" },
    { id: "docs", label: "search_docs", x: 405, y: 116 },
    { id: "tickets", label: "search_tickets", x: 405, y: 194 },
    { id: "web", label: "search_web", x: 405, y: 272 },
    { id: "merge", label: "merge", x: 585, y: MAIN },
    { id: "review", label: "review", x: 708, y: MAIN, kind: "person" },
    { id: "reply", label: "reply", x: 838, y: MAIN, kind: "model" },
  ],
  edges: [
    { id: "e1", from: "request", to: "classify" },
    { id: "e2", from: "classify", to: "escalate", final: "skip" },
    { id: "e3", from: "classify", to: "docs" },
    { id: "e4", from: "classify", to: "tickets" },
    { id: "e5", from: "classify", to: "web" },
    { id: "e6", from: "docs", to: "merge" },
    { id: "e7", from: "tickets", to: "merge" },
    { id: "e8", from: "web", to: "merge" },
    { id: "e9", from: "merge", to: "review" },
    { id: "e10", from: "review", to: "reply" },
  ],
  beats: [
    { at: 0, set: { request: "run" }, log: "run started on thread ticket-7", tone: "run" },
    { at: 700, set: { request: "done", e1: "run", classify: "run" }, log: "step 1: classify asks the model", tone: "run" },
    {
      at: 1800,
      set: { classify: "done", e1: "done", e2: "skip", escalate: "skip", e3: "run", e4: "run", e5: "run", docs: "run", tickets: "run", web: "run" },
      log: "router: three searches at once, escalate skipped",
      tone: "run",
    },
    { at: 2700, set: { docs: "done", e3: "done" } },
    { at: 3100, set: { web: "done", e5: "done" } },
    { at: 3600, set: { tickets: "done", e4: "done" }, log: "step 2 saved: three results, all or nothing", tone: "done" },
    { at: 3800, set: { e6: "run", e7: "run", e8: "run", merge: "run" } },
    {
      at: 4700,
      set: { merge: "done", e6: "done", e7: "done", e8: "done", e9: "wait", review: "wait" },
      log: "paused: the refund needs a person",
      tone: "wait",
      ask: {
        prompt: "Refund 40.00 on order A-1001?",
        approve: [
          { at: 0, set: { review: "done", e9: "done", e10: "run", reply: "run" }, log: "approved by you; the run resumes", tone: "done" },
          { at: 1100, set: { reply: "done", e10: "done" }, log: "completed in 4 steps, every one saved", tone: "done" },
        ],
        reject: [
          { at: 0, set: { review: "fail", e9: "fail", e10: "run", reply: "run" }, log: "rejected by you; no refund is made", tone: "fail" },
          { at: 1100, set: { reply: "done", e10: "done" }, log: "completed: reply explains the decision", tone: "done" },
        ],
      },
    },
  ],
};

export const crash: SceneData = {
  id: "scene-crash",
  title: "A crash, then retry",
  description:
    "An agent calls two tools; the process is killed while one of them runs. A new process calls retry: the finished tool is not run again, the interrupted one runs, and the agent answers.",
  width: 880,
  height: 240,
  nodes: [
    { id: "send", label: "thread.send", x: 80, y: 120, kind: "client" },
    { id: "model", label: "model", x: 250, y: 120, kind: "model" },
    { id: "order", label: "get_order", x: 440, y: 55 },
    { id: "email", label: "send_email", x: 440, y: 185 },
    { id: "model2", label: "model", x: 630, y: 120, kind: "model" },
    { id: "answer", label: "answer", x: 800, y: 120, kind: "client" },
  ],
  edges: [
    { id: "c1", from: "send", to: "model" },
    { id: "c2", from: "model", to: "order" },
    { id: "c3", from: "model", to: "email" },
    { id: "c4", from: "order", to: "model2" },
    { id: "c5", from: "email", to: "model2" },
    { id: "c6", from: "model2", to: "answer" },
  ],
  beats: [
    { at: 0, set: { send: "run" }, log: 'thread.send("Where is my refund?")', tone: "run" },
    { at: 600, set: { send: "done", c1: "run", model: "run" } },
    { at: 1500, set: { model: "done", c1: "done", c2: "run", c3: "run", order: "run", email: "run" }, log: "the model calls two tools at once", tone: "run" },
    { at: 2400, set: { order: "done", c2: "done" }, log: "get_order finished: its result is saved", tone: "done" },
    { at: 3200, set: { email: "fail", c3: "fail" }, flash: "process killed", log: "the process dies while send_email runs", tone: "fail" },
    { at: 5000, set: { email: "run", c3: "run" }, log: "a new process calls thread.retry()", tone: "run" },
    { at: 5300, log: "get_order is not run again: its saved result is used" },
    { at: 6300, set: { email: "done", c3: "done", c4: "run", c5: "run", model2: "run" }, log: "send_email finished; the step is saved", tone: "done" },
    { at: 7200, set: { model2: "done", c4: "done", c5: "done", c6: "run", answer: "run" } },
    { at: 7800, set: { answer: "done", c6: "done" }, log: "answered; finished tools never ran twice", tone: "done" },
  ],
};

export const gateway: SceneData = {
  id: "scene-gateway",
  title: "The gateway",
  description:
    "Two apps call the gateway in the OpenAI format. The first request's route tries OpenAI, gets rate limited, and falls back to a self-hosted Ollama model; the second goes to Anthropic.",
  width: 880,
  height: 260,
  nodes: [
    { id: "py", label: "app.py", x: 150, y: 70, kind: "client" },
    { id: "ts", label: "app.ts", x: 150, y: 190, kind: "client" },
    { id: "gw", label: "gateway", x: 420, y: 130, kind: "gateway" },
    { id: "openai", label: "openai", x: 700, y: 50, kind: "provider", final: "fail" },
    { id: "claude", label: "anthropic", x: 700, y: 130, kind: "provider" },
    { id: "ollama", label: "ollama (yours)", x: 700, y: 210, kind: "provider" },
  ],
  edges: [
    { id: "g1", from: "py", to: "gw" },
    { id: "g2", from: "ts", to: "gw" },
    { id: "g3", from: "gw", to: "openai", final: "fail" },
    { id: "g4", from: "gw", to: "claude" },
    { id: "g5", from: "gw", to: "ollama" },
  ],
  beats: [
    { at: 0, set: { py: "run" }, log: 'app.py: POST /v1/chat/completions, model "support"', tone: "run" },
    { at: 600, set: { g1: "run", gw: "run" }, log: 'route "support": openai, then ollama' },
    { at: 1300, set: { g3: "run", openai: "run" } },
    { at: 2300, set: { openai: "fail", g3: "fail" }, log: "openai answers 429: rate limited", tone: "fail" },
    { at: 2700, set: { g5: "run", ollama: "run" }, log: "falls back to ollama on your own server", tone: "run" },
    { at: 3800, set: { ollama: "done", g5: "done", g1: "done", py: "done" }, log: "200 OK, usage recorded", tone: "done" },
    { at: 4600, set: { ts: "run", g2: "run" }, log: 'app.ts: same endpoint, model "claude"', tone: "run" },
    { at: 5300, set: { g4: "run", claude: "run" } },
    { at: 6400, set: { claude: "done", g4: "done", g2: "done", ts: "done", gw: "done" }, log: "200 OK, in the OpenAI format", tone: "done" },
  ],
};
