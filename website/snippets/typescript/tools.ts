// Docs: guides/tools. Type-checked against the SDK's declarations.

// [show hints]
import { tool } from "criogen";

const scheduleVisit = tool<{
  patientRef: string;
  kind: "home" | "clinic" | "video";
  slots: string[];
  durationMinutes?: number;
  note?: string | null;
}>({
  name: "schedule_visit",
  description: "Book a visit in the first free slot. Slots are ISO 8601 times.",
  input: {
    type: "object",
    properties: {
      patientRef: { type: "string" },
      kind: { enum: ["home", "clinic", "video"] },
      slots: { type: "array", items: { type: "string" }, minItems: 1 },
      durationMinutes: { type: "integer", minimum: 5, maximum: 240 },
      note: { anyOf: [{ type: "string" }, { type: "null" }] },
    },
    required: ["patientRef", "kind", "slots"],
    additionalProperties: false,
  },
  // The type parameter above is what `run` receives: keep it in step with the schema.
  run: ({ slots, kind, durationMinutes = 30 }) => ({ booked: slots[0], kind, minutes: durationMinutes }),
});
// [/show hints]

// [show errors]
import { ToolError } from "criogen";

const getLabResult = tool<{ testId: string }>({
  name: "get_lab_result",
  description: "Read one lab result by its id.",
  readOnly: true,
  input: { type: "object", properties: { testId: { type: "string" } }, required: ["testId"] },
  run: async ({ testId }) => {
    // Goes back to the model as the tool's result; the run goes on.
    if (!testId.startsWith("LAB-")) throw new ToolError("Lab ids look like LAB-12345");
    // retryable: calling again may work. A readOnly or idempotent tool is
    // then retried by the engine before the model sees the error.
    if (await labSystemIsDown()) throw new ToolError("The lab system is not answering", { retryable: true });
    return { testId, status: "final" };
  },
});
// [/show errors]

async function labSystemIsDown(): Promise<boolean> {
  return false;
}
async function outboxPut(_key: string, _text: string): Promise<void> {}

// [show context]
const sendMessage = tool<{ text: string }>({
  name: "send_message",
  description: "Send a message to the care team.",
  destructive: true,
  input: { type: "object", properties: { text: { type: "string" } }, required: ["text"] },
  // The second argument is filled by Criogen. callId is stable across
  // retries of the same call: use it as an idempotency key.
  run: async ({ text }, ctx) => {
    await outboxPut(ctx.callId, text);
    return "sent";
  },
});
// [/show context]

// [show retry]
import { Agent } from "criogen";

const agent = new Agent({
  model: "openai:gpt-5-mini",
  tools: [getLabResult, sendMessage, scheduleVisit],
  // Read-only and idempotent tools are retried after a retryable ToolError
  // or a timeout; other tools never are. This sets how often and how patiently.
  toolRetry: { maxAttempts: 4, initialBackoffSecs: 1 },
  limits: { toolTimeoutSecs: 20 }, // each call; the default is 60
  onToolException: "returnGeneric", // unexpected exceptions: hide their text from the model
});
// [/show retry]
export {};
