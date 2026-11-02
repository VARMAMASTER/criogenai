// A NestJS service around one Criogen agent: built once, on first use, with
// tools, limits, redaction and a fallback; a controller and a health check use it.
import { Injectable, OnModuleInit, ServiceUnavailableException } from "@nestjs/common";
import { Agent, NativeUnavailableError, loadEnv, openai, kaggle, ready, tool, type RunResult } from "criogen";

const findAppointment = tool({
  name: "find_appointment",
  description: "Next appointment for a booking reference.",
  input: { type: "object", properties: { reference: { type: "string" } }, required: ["reference"], additionalProperties: false },
  run: async ({ reference }: { reference: string }) => ({ reference, when: "2026-11-10T10:30", clinic: "ENT" }),
});

@Injectable()
export class AgentService implements OnModuleInit {
  private agent?: Agent;

  onModuleInit(): void {
    loadEnv(); // KAGGLE_API_TOKEN, OPENAI_API_KEY ... from .env (names only are returned)
  }

  private get(): Agent {
    try {
      this.agent ??= new Agent({
        model: kaggle("google/gemini-3.5-flash-lite"),
        fallbacks: process.env.OPENAI_API_KEY ? [openai("gpt-5-mini")] : [],
        tools: [findAppointment],
        limits: { modelCalls: 8, tokens: 40_000 },
        redact: (text) => text.replace(/\b[A-Z]{2}\d{6}\b/g, "[ID]"),
      });
      return this.agent;
    } catch (error) {
      if (error instanceof NativeUnavailableError) throw new ServiceUnavailableException(error.message);
      throw error;
    }
  }

  ask(question: string, signal?: AbortSignal): Promise<RunResult> {
    return this.get().run(question, { signal: signal ?? AbortSignal.timeout(30_000) });
  }

  health() {
    return ready(); // add { model } to check the provider too
  }
}
