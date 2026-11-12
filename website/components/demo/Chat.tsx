"use client";
// Try it: a chat with a support agent built on the real CriogenAI SDK. The
// agent runs in demos/feature-tour/chat_server.py on this computer (it needs
// no account here and keeps model keys out of the browser); the panel beside
// the chat shows what the SDK did: tool calls, the approval gate, memory and
// the audit trail.
import { useCallback, useEffect, useRef, useState } from "react";

/** Where chat_server.py listens (it only accepts this site's pages). */
const SERVER = "http://127.0.0.1:4700";

interface Info {
  models: { id: string; label: string }[];
  orders: string[];
}
interface ToolRun {
  tool: string;
  args?: Record<string, unknown>;
  status?: string;
}
interface Approval {
  call_id: string;
  tool: string;
  args: Record<string, unknown>;
}
interface Msg {
  role: "user" | "assistant" | "notice";
  text: string;
  tools: ToolRun[];
}
interface Panel {
  audit: { action: string; actor: string | null }[];
  audit_ok: boolean;
  memory: { key: string; value: string }[];
}
type ServerEvent =
  | { e: "text"; text: string }
  | { e: "tool_start"; tool: string; args?: Record<string, unknown> }
  | { e: "tool_end"; tool: string; status: string }
  | { e: "approval"; calls: Approval[] }
  | { e: "done"; text: string; usage?: Record<string, number>; model_calls?: number }
  | { e: "error"; code: string; message: string };

const SUGGESTIONS = [
  "Where is order A-1001?",
  "Refund 24 dollars on order A-1001, it arrived scratched.",
  "I'm vegetarian, please remember that.",
  "What do you know about me?",
  "Refund 300 dollars on order A-1001.",
];

const uid = () => Math.random().toString(36).slice(2, 10);

function visitorId(): string {
  try {
    const saved = localStorage.getItem("criogenai-demo-visitor");
    if (saved) return saved;
    const fresh = uid();
    localStorage.setItem("criogenai-demo-visitor", fresh);
    return fresh;
  } catch {
    return uid();
  }
}

export default function Chat() {
  const [info, setInfo] = useState<Info | null>(null);
  const [probed, setProbed] = useState(false);
  const [model, setModel] = useState("");
  const [visitor, setVisitor] = useState("");
  const [thread, setThread] = useState("");
  const [msgs, setMsgs] = useState<Msg[]>([]);
  const [approvals, setApprovals] = useState<Approval[]>([]);
  const [editAmount, setEditAmount] = useState("");
  const [input, setInput] = useState("");
  const [busy, setBusy] = useState(false);
  const [panel, setPanel] = useState<Panel | null>(null);
  const [usage, setUsage] = useState<string>("");
  const abort = useRef<AbortController | null>(null);
  const log = useRef<HTMLDivElement>(null);

  useEffect(() => {
    setVisitor(visitorId());
    setThread(uid());
    const ctl = new AbortController();
    const timer = setTimeout(() => ctl.abort(), 1500);
    fetch(`${SERVER}/api/info`, { signal: ctl.signal })
      .then((r) => (r.ok ? (r.json() as Promise<Info>) : Promise.reject(new Error("no"))))
      .then((i) => {
        setInfo(i);
        setModel(i.models[0]?.id ?? "");
      })
      .catch(() => undefined)
      .finally(() => {
        clearTimeout(timer);
        setProbed(true);
      });
    return () => {
      clearTimeout(timer);
      ctl.abort();
    };
  }, []);

  useEffect(() => {
    log.current?.scrollTo({ top: log.current.scrollHeight });
  }, [msgs, approvals]);

  const refreshPanel = useCallback(
    async (t: string, v: string) => {
      try {
        const r = await fetch(`${SERVER}/api/state?thread=${encodeURIComponent(t)}&visitor=${encodeURIComponent(v)}`);
        if (r.ok) setPanel((await r.json()) as Panel);
      } catch {
        /* the panel is a convenience */
      }
    },
    [],
  );

  /** POST to the server and apply its NDJSON events to the last assistant message. */
  const call = async (path: string, body: Record<string, unknown>) => {
    setBusy(true);
    setApprovals([]);
    setUsage("");
    setMsgs((m) => [...m, { role: "assistant", text: "", tools: [] }]);
    const ctl = new AbortController();
    abort.current = ctl;
    const patch = (fn: (m: Msg) => Msg) => setMsgs((all) => all.map((m, i) => (i === all.length - 1 ? fn(m) : m)));
    try {
      const res = await fetch(`${SERVER}${path}`, {
        method: "POST",
        body: JSON.stringify({ thread, visitor, model, ...body }),
        signal: ctl.signal,
      });
      if (!res.ok || !res.body) throw new Error(((await res.json().catch(() => ({}))) as { error?: string }).error ?? `HTTP ${res.status}`);
      const reader = res.body.getReader();
      const decoder = new TextDecoder();
      let buffer = "";
      for (;;) {
        const { done, value } = await reader.read();
        if (done) break;
        buffer += decoder.decode(value, { stream: true });
        const lines = buffer.split("\n");
        buffer = lines.pop() ?? "";
        for (const line of lines) {
          if (!line.trim()) continue;
          const ev = JSON.parse(line) as ServerEvent;
          if (ev.e === "text") patch((m) => ({ ...m, text: m.text + ev.text }));
          else if (ev.e === "tool_start") patch((m) => ({ ...m, tools: [...m.tools, { tool: ev.tool, args: ev.args }] }));
          else if (ev.e === "tool_end")
            patch((m) => ({ ...m, tools: m.tools.map((t) => (t.tool === ev.tool && !t.status ? { ...t, status: ev.status } : t)) }));
          else if (ev.e === "approval") {
            setApprovals(ev.calls);
            setEditAmount(String(ev.calls[0]?.args?.amount ?? ""));
          } else if (ev.e === "error") patch((m) => ({ ...m, role: "notice", text: `${ev.code}: ${ev.message}` }));
          else if (ev.e === "done" && ev.usage) setUsage(`${ev.model_calls ?? 0} model call(s), ${ev.usage.input_tokens ?? 0} tokens in, ${ev.usage.output_tokens ?? 0} out`);
        }
      }
    } catch (e) {
      if ((e as Error).name !== "AbortError") patch((m) => ({ ...m, role: "notice", text: (e as Error).message }));
    }
    // Drop an assistant bubble that stayed empty (a turn that only asked for approval).
    setMsgs((all) => all.filter((m, i) => !(i === all.length - 1 && m.role === "assistant" && !m.text && m.tools.length === 0)));
    setBusy(false);
    refreshPanel(thread, visitor);
  };

  const send = (text: string) => {
    const t = text.trim();
    if (!t || busy) return;
    setInput("");
    setMsgs((m) => [...m, { role: "user", text: t, tools: [] }]);
    void call("/api/chat", { message: t });
  };

  const newConversation = () => {
    abort.current?.abort();
    setThread(uid());
    setMsgs([]);
    setApprovals([]);
    setPanel(null);
    setUsage("");
    setBusy(false);
  };

  if (!probed) return <p className="lab-muted">Looking for the local demo server…</p>;
  if (!info) {
    return (
      <div>
        <p className="lab-muted">
          The chat runs a real CriogenAI agent, so it needs the demo server on your computer (it keeps your model keys out of the browser). Start it, then
          reload this page:
        </p>
        <pre className="demo-cmd">python demos/feature-tour/chat_server.py   # http://127.0.0.1:4700</pre>
        <p className="lab-muted pg-small">
          It reads provider keys from <code>~/.criogen-keys.env</code> (Gemini, OpenRouter or OpenAI), listens on 127.0.0.1 only, and accepts calls from this
          site&apos;s pages only. The scenarios in the other tabs show everything the chat can do, recorded.
        </p>
      </div>
    );
  }
  if (info.models.length === 0) {
    return <p className="lab-muted">The demo server is running but found no model key. Add one to ~/.criogen-keys.env (GEMINI_API_KEY, OPENROUTER_API_KEY or OPENAI_API_KEY) and restart it.</p>;
  }

  const pending = approvals[0];
  return (
    <div className="chat">
      <div className="chat-main">
        <div className="chat-bar">
          <label className="pg-field">
            <span>Model</span>
            <select value={model} onChange={(e) => setModel(e.target.value)} disabled={busy}>
              {info.models.map((m) => (
                <option key={m.id} value={m.id}>
                  {m.label}
                </option>
              ))}
            </select>
          </label>
          <button type="button" className="lab-button" onClick={newConversation}>
            New conversation
          </button>
        </div>
        <div className="chat-log" ref={log} role="log" aria-live="polite" aria-label="Conversation">
          {msgs.length === 0 && (
            <div className="chat-empty">
              <p className="lab-muted">
                You are talking to a support agent for a small shop (orders {info.orders.join(", ")}). Try:
              </p>
              <div className="chat-chips">
                {SUGGESTIONS.map((s) => (
                  <button key={s} type="button" className="lab-button" onClick={() => send(s)}>
                    {s}
                  </button>
                ))}
              </div>
            </div>
          )}
          {msgs.map((m, i) => (
            <div key={i} className={`chat-msg chat-${m.role}`}>
              {m.tools.map((t, j) => (
                <div key={j} className="chat-tool">
                  <span aria-hidden="true">{t.status === "error" ? "✗" : t.status ? "✓" : "…"}</span> {t.tool}
                  {t.args ? `(${Object.entries(t.args).map(([k, v]) => `${k}=${JSON.stringify(v)}`).join(", ")})` : ""}
                  {t.status ? ` · ${t.status}` : " · running"}
                </div>
              ))}
              {m.text && <div className="chat-text">{m.text}</div>}
              {!m.text && m.role === "assistant" && busy && i === msgs.length - 1 && m.tools.length === 0 && <div className="chat-text lab-muted">…</div>}
            </div>
          ))}
          {pending && (
            <div className="chat-approval" role="group" aria-label="Approval needed">
              <b>A person must approve this before it runs</b>
              <div className="chat-call">
                {pending.tool}({Object.entries(pending.args).map(([k, v]) => `${k}=${JSON.stringify(v)}`).join(", ")})
              </div>
              <div className="chat-actions">
                <button type="button" className="lab-button pg-primary" disabled={busy} onClick={() => call("/api/approve", {})}>
                  Approve
                </button>
                <button type="button" className="lab-button" disabled={busy} onClick={() => call("/api/reject", { reason: "Rejected by the reviewer." })}>
                  Reject
                </button>
                {pending.tool === "issue_refund" && (
                  <span className="chat-edit">
                    <label>
                      <span className="chat-sr">Amount</span>
                      <input type="number" min={1} value={editAmount} onChange={(e) => setEditAmount(e.target.value)} aria-label="Edited amount in dollars" />
                    </label>
                    <button
                      type="button"
                      className="lab-button"
                      disabled={busy || !(Number(editAmount) > 0)}
                      onClick={() => call("/api/edit", { call_id: pending.call_id, args: { ...pending.args, amount: Number(editAmount) } })}
                    >
                      Edit amount and approve
                    </button>
                  </span>
                )}
              </div>
            </div>
          )}
        </div>
        <form
          className="chat-form"
          onSubmit={(e) => {
            e.preventDefault();
            send(input);
          }}
        >
          <label className="chat-sr" htmlFor="chat-input">
            Message
          </label>
          <input id="chat-input" value={input} onChange={(e) => setInput(e.target.value)} placeholder={pending ? "Decide on the request above first" : "Ask about an order, ask for a refund…"} disabled={busy || !!pending} maxLength={2000} autoComplete="off" />
          <button type="submit" className="lab-button pg-primary" disabled={busy || !!pending || !input.trim()}>
            Send
          </button>
        </form>
        {usage && <p className="lab-muted pg-small">{usage}</p>}
      </div>

      <aside className="chat-side" aria-label="What CriogenAI did">
        <h3>Under the hood</h3>
        <section>
          <h4>Approval gate</h4>
          <p className="pg-small lab-muted">
            <code>issue_refund</code> never runs until you decide above. Refunds over $200 are then stopped by a hook, and the model is told why.
          </p>
        </section>
        <section>
          <h4>Memory (this visitor)</h4>
          {panel && panel.memory.length > 0 ? (
            <ul className="chat-list">
              {panel.memory.map((f) => (
                <li key={f.key}>
                  <b>{f.key}</b>: {f.value}
                </li>
              ))}
            </ul>
          ) : (
            <p className="pg-small lab-muted">Nothing saved yet. Tell the agent something about you.</p>
          )}
        </section>
        <section>
          <h4>Audit trail {panel && <span className={panel.audit_ok ? "demo-ok" : "demo-fail"}>{panel.audit_ok ? "✓ chain verified" : "✗ chain broken"}</span>}</h4>
          {panel && panel.audit.length > 0 ? (
            <ol className="chat-list chat-audit">
              {panel.audit.map((a, i) => (
                <li key={i}>
                  <code>{a.action}</code> <span className="lab-muted">{a.actor}</span>
                </li>
              ))}
            </ol>
          ) : (
            <p className="pg-small lab-muted">Records appear as the agent works. They contain no message text.</p>
          )}
        </section>
        <p className="pg-small lab-muted">
          The conversation is saved to SQLite after every step. A model that is rate limited falls back to another one you have a key for.
        </p>
      </aside>
    </div>
  );
}
