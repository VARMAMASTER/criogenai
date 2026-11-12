# Feature tour and demo chat

The website's `/demo/` page is built from this folder.

| File | What it does |
|---|---|
| `tour.py` | Runs every feature on the status page as ordinary SDK code and records the code, its output and the checks it passed into `website/public/demo/`. |
| `chat_server.py` | A support agent on the real SDK (tools, approval gate, memory, audit trail, SQLite, a guard hook) that the page's chat talks to. |

```sh
pip install criogenai
python demos/feature-tour/tour.py                          # scripted model: no network, no keys -> tour.json
python demos/feature-tour/tour.py --live gemini            # a real model -> live.json (also: openai, openrouter, openrouter-free)
python demos/feature-tour/chat_server.py                   # the chat backend, http://127.0.0.1:4700
```

Keys are read from `~/.criogen-keys.env` into the process only; they are never written to the recorded files or sent to the browser.
A real model varies, so live scenarios check behaviour (which tool ran, what paused, what was saved), never wording, and get one retry that the
page reports. The chat server listens on 127.0.0.1 and accepts calls from the website's pages only.
