"use client";
// Browser side of app/api/agent/stream: plain fetch, no criogenai import (the
// engine stays on the server). Copy this hook into your React app.
//
//   const { answer, tools, busy, error, ask, stop } = useAgentStream();
//   <button onClick={() => ask("Where is order 42?")}>Ask</button> <p>{answer}</p>
import { useCallback, useRef, useState } from "react";

export type ToolState = { tool: string; state: string };

export function useAgentStream(url = "/api/agent/stream") {
  const [answer, setAnswer] = useState("");
  const [tools, setTools] = useState<ToolState[]>([]);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const controller = useRef<AbortController | null>(null);

  const stop = useCallback(() => controller.current?.abort(), []);

  const ask = useCallback(
    async (question: string, conversationId?: string) => {
      controller.current?.abort();
      const abort = new AbortController();
      controller.current = abort;
      setAnswer("");
      setTools([]);
      setError(null);
      setBusy(true);
      try {
        const response = await fetch(url, {
          method: "POST",
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify({ question, conversationId }),
          signal: abort.signal,
        });
        if (!response.ok || !response.body) throw new Error(`HTTP ${response.status}`);
        const reader = response.body.pipeThrough(new TextDecoderStream()).getReader();
        let buffer = "";
        for (;;) {
          const { value, done } = await reader.read();
          if (done) break;
          buffer += value;
          // Events end with a blank line; keep a partial one for the next chunk.
          const events = buffer.split("\n\n");
          buffer = events.pop() ?? "";
          for (const raw of events) {
            if (!raw.startsWith("data: ")) continue;
            const event = JSON.parse(raw.slice(6));
            if (event.type === "text") setAnswer((text) => text + event.text);
            else if (event.type === "tool") setTools((list) => [...list, { tool: event.tool, state: event.state }]);
            else if (event.type === "error") setError(event.code);
          }
        }
      } catch (e) {
        if (!abort.signal.aborted) setError(e instanceof Error ? e.message : String(e));
      } finally {
        if (controller.current === abort) setBusy(false);
      }
    },
    [url],
  );

  return { answer, tools, busy, error, ask, stop };
}
