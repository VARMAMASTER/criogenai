# Continuous chat

A small chat that **keeps its conversation**: close it, open it tomorrow, and
the model still knows what you said. Built on the private pilot package
`@varmamaster/sdk` (CriogenAI), about 150 lines of code.

- `chat.mjs`: the agent and one saved conversation (a *thread*) per name.
- `cli.mjs`: a terminal chat. `server.mjs` + `index.html`: the same in a browser.
- The conversation is kept in a SQLite file (`chat.db`); nothing else to run.

## Run it

```sh
# 1. Install the pilot package (GitHub Packages, private). The token comes
#    from the GitHub CLI login and is not written anywhere:
GITHUB_PACKAGES_TOKEN="$(gh auth token)" npm install --ignore-scripts

# 2. Give it a model: a .env file next to this README (it is git-ignored)
echo 'GEMINI_API_KEY=your-key' > .env        # or OPENAI_API_KEY=…
#    or no key at all, to try it offline:  CHAT_MODEL=fake

# 3. Chat
npm run cli -- sai          # terminal; the name picks the conversation
npm run web                 # browser: http://localhost:4500
npm test                    # offline tests
```

Try it: tell it your name, type `/quit`, run it again and ask "what is my
name?". Each name has its own conversation; `/new` (or *Forget* in the page)
deletes one.

## Notes

- Windows x64 only for now: the pilot package carries the Windows addon.
- The web server listens on this computer only (`HOST` changes that: put it
  behind your own login first, it has none).
- Everything comes from the SDK's thread: `thread.stream(text)` to chat,
  `thread.messages()` to read the saved messages back (the earlier exchanges
  shown on return), `thread.history()` for its saved steps, `thread.delete()`
  to forget. There is no second copy of the conversation.
