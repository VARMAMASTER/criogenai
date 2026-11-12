# Mugshot Support: an CriogenAI demo

A small but real app on CriogenAI: a customer support agent for a pretend
coffee gear shop, with a Next.js chat UI.

What it shows:

- **Threads in SQLite.** Every conversation is an CriogenAI thread in
  `server/support.db`. Restart the server and conversations are still there.
- **Human approval.** The agent may call `issue_refund`, but the call waits
  until you press *Approve* or *Reject* in the UI. The pending approval is
  stored, so it survives a restart too.
- **Crash recovery.** Runs are saved before every step and after every tool
  call. Stop the server while the agent is working, start it again, and the
  UI offers *Continue the run*; finished tool calls are not repeated.
- **Any model, same thread.** Pick OpenAI or Gemini per message; both read
  the same stored conversation.
- **Checkpoints and rewind.** The right panel lists the thread's
  checkpoints; *rewind here* makes an older one current.

```
browser ──/api/*──▶ Next.js (web, :3000) ──rewrite──▶ Rust server (server, :8787) ──▶ OpenAI / Gemini
                                                          │
                                                          └── SQLite: support.db
```

The browser never sees an API key: only the Rust server talks to the model
provider.

## Run it

Needs Rust and Node.js 20.9 or newer.

1. Put your keys in `~/.criogen-keys.env` (outside the repository):

   ```
   OPENAI_API_KEY=sk-...
   GEMINI_API_KEY=...
   ```

   Either one is enough. Environment variables of the same name take
   precedence; `CRIOGEN_KEYS_FILE` points to another file.
   Optional: `OPENAI_MODEL` (default `gpt-5-mini`), `GEMINI_MODEL` (default
   `gemini-2.5-flash`), `SUPPORT_DB` (default `sqlite://support.db`), `PORT`
   (default 8787).

2. Start the server:

   ```sh
   cd demos/support-chat/server
   cargo run
   ```

3. Start the UI in a second terminal:

   ```sh
   cd demos/support-chat/web
   npm install
   npm run dev
   ```

4. Open http://localhost:3000. Try "My mug from order A-1001 arrived broken.
   Can I get my money back?"

## Things to try

- Ask for a refund, then stop the server (Ctrl+C) before approving. Start
  it again and reload: the approval is still waiting.
- Approve, and stop the server while the agent writes its answer. After a
  restart the conversation shows *Continue the run*; the refund is not paid
  twice (the right panel counts payments).
- Switch the model between messages.
- Rewind to an earlier checkpoint and ask something else.

The orders (A-1001, A-1002, A-1003) and stock levels are fixed sample data
in `server/src/main.rs`. This demo calls real models, so it is not part of
CI; the framework's own tests use simulated models.
