"use client";

import { useCallback, useEffect, useRef, useState } from "react";

// ---- API shapes (see ../server/src/main.rs) --------------------------------

type Block =
  | { type: "text"; text: string }
  | { type: "tool_call"; id: string; name: string; args: Record<string, unknown> }
  | { type: string; [key: string]: unknown };

type Message = {
  id: string;
  role: "user" | "assistant" | "tool" | "system";
  content: Block[];
  tool?: string;
  call_id?: string;
  status?: "success" | "error" | "rejected" | string;
  error?: { message?: string };
};

type ApprovalRequest = {
  call_id: string;
  tool: string;
  args: Record<string, unknown>;
  description?: string;
  allowed: string[];
};

type CheckpointMeta = {
  id: string;
  version: number;
  source: string;
  status: string;
  step: number;
  created_at_ms: number;
};

type Last = {
  ok: boolean;
  provider: string;
  model: string;
  elapsed_ms: number;
  model_calls?: number;
  tool_calls?: number;
  usage?: { input_tokens: number; output_tokens: number };
  error?: { code: string; message: string };
};

type View = {
  id: string;
  status: "completed" | "suspended" | "failed" | "in_progress" | null;
  version: number | null;
  messages: Message[];
  approvals: ApprovalRequest[];
  history: CheckpointMeta[];
  last: Last | null;
  refunds_paid: number;
};

type Provider = { id: string; label: string; model: string };
type ThreadEntry = { id: string; title: string };

class ApiError extends Error {
  constructor(public code: string, message: string) {
    super(message);
  }
}

async function api<T>(path: string, method = "GET", body?: unknown): Promise<T> {
  const response = await fetch(`/api${path}`, {
    method,
    headers: body === undefined ? undefined : { "content-type": "application/json" },
    body: body === undefined ? undefined : JSON.stringify(body),
  });
  const data = await response.json().catch(() => null);
  if (!response.ok) {
    const error = data?.error ?? { code: `http_${response.status}`, message: "the server is not reachable" };
    throw new ApiError(error.code, error.message);
  }
  return data as T;
}

const STORE_KEY = "mugshot-threads";
const SUGGESTIONS = [
  "Where is my order A-1002?",
  "My mug from order A-1001 arrived broken. Can I get my money back?",
  "Is the burr grinder in stock? I ordered one (A-1003).",
];

function loadThreads(): ThreadEntry[] {
  try {
    return JSON.parse(localStorage.getItem(STORE_KEY) ?? "[]");
  } catch {
    return [];
  }
}

function saveThreads(threads: ThreadEntry[]) {
  try {
    localStorage.setItem(STORE_KEY, JSON.stringify(threads));
  } catch {
    // Private mode: the list lives for this tab only.
  }
}

function newThreadId() {
  return `chat-${Date.now().toString(36)}-${Math.random().toString(36).slice(2, 6)}`;
}

function text(message: Message) {
  return message.content
    .filter((b): b is { type: "text"; text: string } => b.type === "text")
    .map((b) => b.text)
    .join("\n");
}

// ---- page -----------------------------------------------------------------

export default function Page() {
  const [providers, setProviders] = useState<Provider[]>([]);
  const [provider, setProvider] = useState<string>("");
  const [threads, setThreads] = useState<ThreadEntry[]>([]);
  const [current, setCurrent] = useState<string>("");
  const [view, setView] = useState<View | null>(null);
  const [busy, setBusy] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [input, setInput] = useState("");
  const [reason, setReason] = useState("");
  const bottom = useRef<HTMLDivElement>(null);

  useEffect(() => {
    api<{ providers: Provider[]; default: string }>("/config")
      .then((config) => {
        setProviders(config.providers);
        setProvider(config.default);
      })
      .catch((e: ApiError) => setError(`Server: ${e.message}. Is the Rust server running?`));
    const saved = loadThreads();
    setThreads(saved);
    setCurrent(saved[0]?.id ?? newThreadId());
  }, []);

  const refresh = useCallback(async (id: string) => {
    try {
      setView(await api<View>(`/threads/${id}`));
    } catch (e) {
      setError((e as Error).message);
    }
  }, []);

  useEffect(() => {
    if (current) refresh(current);
  }, [current, refresh]);

  useEffect(() => {
    bottom.current?.scrollIntoView({ behavior: "smooth" });
  }, [view?.messages.length, busy]);

  const remember = (id: string, title: string) => {
    setThreads((list) => {
      if (list.some((t) => t.id === id)) return list;
      const next = [{ id, title: title.slice(0, 60) }, ...list];
      saveThreads(next);
      return next;
    });
  };

  const call = async (label: string, path: string, body?: unknown) => {
    setBusy(label);
    setError(null);
    try {
      const next = await api<View>(`/threads/${current}${path}`, "POST", body);
      setView(next);
      if (next.last?.error) setError(`${next.last.error.code}: ${next.last.error.message}`);
    } catch (e) {
      const err = e as ApiError;
      setError(`${err.code}: ${err.message}`);
      await refresh(current);
    } finally {
      setBusy(null);
    }
  };

  const send = async (message: string) => {
    if (!message.trim() || busy) return;
    remember(current, message);
    setInput("");
    // Show the message at once; the server's view replaces it.
    setView((v) =>
      v && {
        ...v,
        messages: [...v.messages, { id: "pending", role: "user", content: [{ type: "text", text: message }] }],
      },
    );
    await call("Thinking", "/messages", { text: message, provider });
  };

  const newChat = () => {
    setCurrent(newThreadId());
    setError(null);
  };

  const deleteChat = async (id: string) => {
    await api(`/threads/${id}`, "DELETE").catch(() => null);
    const next = threads.filter((t) => t.id !== id);
    setThreads(next);
    saveThreads(next);
    if (id === current) setCurrent(next[0]?.id ?? newThreadId());
  };

  const status = view?.status ?? null;
  const waiting = status === "suspended";
  const stuck = status === "failed" || status === "in_progress";

  return (
    <div className="app">
      <aside className="sidebar">
        <div className="brand">
          <span className="logo">☕</span>
          <div>
            <strong>Mugshot Support</strong>
            <small>Criogen demo</small>
          </div>
        </div>
        <button className="primary wide" onClick={newChat}>
          New conversation
        </button>
        <label className="field">
          Model
          <select value={provider} onChange={(e) => setProvider(e.target.value)}>
            {providers.map((p) => (
              <option key={p.id} value={p.id}>
                {p.label} · {p.model}
              </option>
            ))}
          </select>
        </label>
        <nav className="threads">
          {threads.map((t) => (
            <div key={t.id} className={`thread ${t.id === current ? "active" : ""}`}>
              <button className="link" onClick={() => setCurrent(t.id)} title={t.id}>
                {t.title}
              </button>
              <button className="icon" onClick={() => deleteChat(t.id)} aria-label="Delete conversation">
                ×
              </button>
            </div>
          ))}
          {threads.length === 0 && <p className="muted">No conversations yet.</p>}
        </nav>
      </aside>

      <main className="chat">
        <div className="messages">
          {view && view.messages.length === 0 && (
            <div className="empty">
              <h1>How can we help?</h1>
              <p className="muted">
                Orders A-1001, A-1002 and A-1003 exist. Refunds wait for your approval.
              </p>
              <div className="suggestions">
                {SUGGESTIONS.map((s) => (
                  <button key={s} onClick={() => send(s)}>
                    {s}
                  </button>
                ))}
              </div>
            </div>
          )}
          {view?.messages.map((m) => <MessageView key={m.id} message={m} />)}
          {busy && <div className="typing">{busy}…</div>}
          <div ref={bottom} />
        </div>

        {waiting && view && (
          <section className="approval">
            <h2>Approval needed</h2>
            {view.approvals.map((a) => (
              <div key={a.call_id} className="request">
                <p>{a.description ?? a.tool}</p>
                <code>{JSON.stringify(a.args)}</code>
              </div>
            ))}
            <div className="row">
              <button className="primary" disabled={!!busy} onClick={() => call("Approving", "/approve", { provider })}>
                Approve
              </button>
              <input
                placeholder="Reason for rejecting (optional)"
                value={reason}
                onChange={(e) => setReason(e.target.value)}
              />
              <button
                className="danger"
                disabled={!!busy}
                onClick={() => {
                  call("Rejecting", "/reject", { message: reason, provider });
                  setReason("");
                }}
              >
                Reject
              </button>
            </div>
          </section>
        )}

        {stuck && (
          <section className="banner">
            {status === "in_progress"
              ? "The last run was interrupted before it finished (for example, the server stopped)."
              : "The last run failed. Its progress is saved."}
            <button className="primary" disabled={!!busy} onClick={() => call("Continuing", "/retry", { provider })}>
              Continue the run
            </button>
          </section>
        )}

        {error && <div className="error">{error}</div>}

        <form
          className="composer"
          onSubmit={(e) => {
            e.preventDefault();
            send(input);
          }}
        >
          <input
            value={input}
            onChange={(e) => setInput(e.target.value)}
            placeholder={waiting ? "Answer the approval first" : stuck ? "Continue the run first" : "Write a message"}
            disabled={!!busy || waiting || stuck}
          />
          <button className="primary" disabled={!!busy || waiting || stuck || !input.trim()}>
            Send
          </button>
        </form>
      </main>

      <aside className="inspector">
        <h2>Last call</h2>
        {view?.last ? (
          <dl>
            <dt>Model</dt>
            <dd>
              {view.last.provider} · {view.last.model}
            </dd>
            <dt>Time</dt>
            <dd>{(view.last.elapsed_ms / 1000).toFixed(1)} s</dd>
            {view.last.ok && (
              <>
                <dt>Calls</dt>
                <dd>
                  {view.last.model_calls} model, {view.last.tool_calls} tool
                </dd>
                <dt>Tokens</dt>
                <dd>
                  {view.last.usage?.input_tokens ?? 0} in, {view.last.usage?.output_tokens ?? 0} out
                </dd>
              </>
            )}
          </dl>
        ) : (
          <p className="muted">Nothing yet.</p>
        )}
        <p className="muted small">Refunds paid by the tool: {view?.refunds_paid ?? 0}</p>

        <h2>Checkpoints</h2>
        <p className="muted small">Saved in SQLite before every step and after every tool call.</p>
        <ol className="history">
          {view?.history.map((c, i) => (
            <li key={c.id} className={i === 0 ? "head" : ""}>
              <span className="version">v{c.version}</span>
              <span className={`tag ${c.source}`}>{c.source}</span>
              <span className={`tag ${c.status}`}>{c.status.replace("_", " ")}</span>
              {i > 0 && c.status === "completed" && (
                <button
                  className="link small"
                  disabled={!!busy}
                  onClick={() => call("Rewinding", "/rewind", { checkpoint_id: c.id })}
                >
                  rewind here
                </button>
              )}
            </li>
          ))}
        </ol>
      </aside>
    </div>
  );
}

function MessageView({ message }: { message: Message }) {
  if (message.role === "tool") {
    // Tool results are text or JSON blocks.
    const body = message.content
      .map((b) => (b.type === "text" ? (b as { text: string }).text : b.type === "json" ? JSON.stringify(b.value, null, 2) : ""))
      .filter(Boolean)
      .join("\n");
    return (
      <details className={`tool-result ${message.status ?? ""}`}>
        <summary>
          {message.status === "rejected" ? "✗ rejected" : message.status === "error" ? "⚠ error" : "✓ result"} ·{" "}
          {message.tool}
        </summary>
        <pre>{body || message.error?.message}</pre>
      </details>
    );
  }
  const calls = message.content.filter((b) => b.type === "tool_call") as {
    name: string;
    args: Record<string, unknown>;
    id: string;
  }[];
  const body = text(message);
  return (
    <div className={`message ${message.role}`}>
      {body && <div className="bubble">{body}</div>}
      {calls.map((c) => (
        <div key={c.id} className="tool-call">
          <span>calls</span> <strong>{c.name}</strong> <code>{JSON.stringify(c.args)}</code>
        </div>
      ))}
    </div>
  );
}
