"""Record playground runs for the website, which replays them when no
playground server is running.

    <python with psutil> playground/record.py [out.json]

Runs each preset below (the real workers, the real mock model server, one
after the other, A-B-B-A) and writes every event, untouched, with the machine
and the date. The website shows these as "recorded on <machine>"; it changes
no number.
"""

import datetime
import json
import sys
from pathlib import Path

import server

PRESETS = [
    {
        "id": "realistic",
        "label": "With a model's waiting time",
        "about": "Each model call waits 200 ms and each tool 100 ms, as a hosted model and a database would.",
        "body": {"message": "Where is my refund for order A-1001?", "tools": 3, "model_delay_ms": 200, "tool_delay_ms": 100,
                 "runs": 30, "concurrency": 1, "order": "sequential"},
    },
    {
        "id": "overhead",
        "label": "Framework cost only",
        "about": "No waiting at all, 100 runs, 10 at a time: what is left is what the frameworks themselves cost.",
        "body": {"message": "Where is my refund for order A-1001?", "tools": 3, "model_delay_ms": 0, "tool_delay_ms": 0,
                 "runs": 100, "concurrency": 10, "order": "sequential"},
    },
]


def main() -> None:
    out = Path(sys.argv[1] if len(sys.argv) > 1 else Path(__file__).parent.parent / "website" / "public" / "playground" / "recorded.json")
    recordings = []
    for preset in PRESETS:
        print("recording", preset["id"], flush=True)
        events: list = []
        server.run_job(server.read_job(preset["body"]), events.append)
        recordings.append({"id": preset["id"], "label": preset["label"], "about": preset["about"], "events": events})
    data = {
        "recorded_at": datetime.datetime.now(datetime.timezone.utc).isoformat(timespec="seconds"),
        "machine": server.machine(),
        "recordings": recordings,
    }
    out.parent.mkdir(parents=True, exist_ok=True)
    out.write_text(json.dumps(data, separators=(",", ":")), encoding="utf-8")
    print(f"wrote {out} ({out.stat().st_size // 1024} KB)")


if __name__ == "__main__":
    main()
