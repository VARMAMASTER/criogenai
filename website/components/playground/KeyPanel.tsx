"use client";
// "Try with your own key": provider, model and key, kept in tab memory
// (KeyProvider). The key field is a password input with autocomplete off,
// and "Forget key" drops it at once.
import { useState } from "react";
import { PROVIDERS, type ProviderId } from "@/lib/patterns/providers";
import { useKey } from "./KeyProvider";

export default function KeyPanel() {
  const { conn, set } = useKey();
  const [provider, setProvider] = useState<ProviderId>(conn?.provider ?? "openai");
  const [model, setModel] = useState(conn?.model ?? PROVIDERS[0].model);
  const [key, setKey] = useState("");
  const info = PROVIDERS.find((p) => p.id === provider)!;

  if (conn) {
    const p = PROVIDERS.find((x) => x.id === conn.provider)!;
    return (
      <div className="sb-key sb-key-on">
        <div>
          <b>Real model on:</b> {p.label} · <code>{conn.model}</code> · key ending in <code>…{conn.key.slice(-4)}</code>
          <p className="sb-small">
            Held in this tab&apos;s memory only. Requests go from your browser straight to {p.label}; this site never sees them. Tools stay simulated.
          </p>
        </div>
        <button type="button" className="lab-button sb-danger" onClick={() => set(null)}>
          Forget key
        </button>
      </div>
    );
  }

  return (
    <details className="sb-key">
      <summary>
        <b>Try it with your own key</b> <span className="sb-small">optional: a real model makes the decisions; the tools stay simulated</span>
      </summary>
      <form
        className="sb-key-form"
        onSubmit={(e) => {
          e.preventDefault();
          if (key.trim()) set({ provider, model: model.trim() || info.model, key: key.trim() });
          setKey("");
        }}
      >
        <label>
          <span>Provider</span>
          <select
            value={provider}
            onChange={(e) => {
              const id = e.target.value as ProviderId;
              setProvider(id);
              setModel(PROVIDERS.find((p) => p.id === id)!.model);
            }}
          >
            {PROVIDERS.map((p) => (
              <option key={p.id} value={p.id}>
                {p.label}
              </option>
            ))}
          </select>
        </label>
        <label>
          <span>Model</span>
          <input value={model} onChange={(e) => setModel(e.target.value)} spellCheck={false} autoComplete="off" />
        </label>
        <label className="sb-grow">
          <span>
            API key (<a href={info.keysUrl} target="_blank" rel="noreferrer">get one</a>)
          </span>
          <input
            type="password"
            value={key}
            onChange={(e) => setKey(e.target.value)}
            placeholder={info.keyHint}
            autoComplete="off"
            spellCheck={false}
            data-1p-ignore
            data-lpignore="true"
          />
        </label>
        <button type="submit" className="lab-button sb-primary" disabled={!key.trim()}>
          Use key
        </button>
      </form>
      <ul className="sb-promise">
        <li>Your key stays in this tab&apos;s memory: not saved, not in cookies or the address bar. Reload or close the tab and it is gone.</li>
        <li>Your browser calls {info.label} directly. Nothing passes through criogen.vercel.app, and nothing is logged.</li>
        <li>Each run makes at most 16 short model calls, billed to your account. Tools, data and side effects are simulated.</li>
      </ul>
    </details>
  );
}
