"""The fake model server both frameworks talk to: the same script for
everyone, in the OpenAI wire format (no framework environments needed).

    python -m unittest discover -s playground
"""

import json
import threading
import time
import unittest
import urllib.request
from http.server import ThreadingHTTPServer

import mock_model

TOOLS = [{"type": "function", "function": {"name": n}} for n in ("get_order", "get_customer")]


class Script(unittest.TestCase):
    def test_it_asks_for_every_tool_at_once_then_answers(self):
        user = {"role": "user", "content": "hi"}
        self.assertEqual(mock_model.script({"messages": [user], "tools": TOOLS}), {"calls": ["get_order", "get_customer"]})
        one = [user, {"role": "assistant"}, {"role": "tool", "content": "x"}]
        self.assertEqual(mock_model.script({"messages": one, "tools": TOOLS}), {"calls": ["get_order", "get_customer"]})
        both = one + [{"role": "tool", "content": "y"}]
        self.assertEqual(mock_model.script({"messages": both, "tools": TOOLS})["text"], "[simulated reply after 2 lookups] You said: “hi”.")

    def test_without_tools_it_answers_at_once_and_quotes_the_message(self):
        step = mock_model.script({"messages": [{"role": "user", "content": [{"type": "text", "text": "where?"}]}]})
        self.assertEqual(step["text"], "[simulated reply] You said: “where?”.")

    def test_only_the_latest_question_counts(self):
        messages = [{"role": "user", "content": "old"}, {"role": "tool", "content": "x"}, {"role": "assistant", "content": "ok"}, {"role": "user", "content": "new"}]
        self.assertEqual(mock_model.script({"messages": messages, "tools": TOOLS}), {"calls": ["get_order", "get_customer"]})

    def test_the_delay_is_in_the_model_name(self):
        self.assertEqual(mock_model.delay_of("mock-250"), 0.25)
        self.assertEqual(mock_model.delay_of("mock-0"), 0.0)
        self.assertEqual(mock_model.delay_of("gpt-5"), 0.0)


class OverHttp(unittest.TestCase):
    def setUp(self):
        self.http = mock_model.Server(("127.0.0.1", 0), mock_model.Handler)
        threading.Thread(target=self.http.serve_forever, daemon=True).start()
        self.url = f"http://127.0.0.1:{self.http.server_address[1]}/v1/chat/completions"

    def tearDown(self):
        self.http.shutdown()

    def ask(self, body):
        request = urllib.request.Request(self.url, data=json.dumps(body).encode(), method="POST")
        return urllib.request.urlopen(request, timeout=10)

    def test_a_plain_reply_has_tool_calls_with_json_arguments(self):
        reply = json.load(self.ask({"model": "mock-0", "messages": [{"role": "user", "content": "hi"}], "tools": TOOLS}))
        choice = reply["choices"][0]
        self.assertEqual(choice["finish_reason"], "tool_calls")
        calls = choice["message"]["tool_calls"]
        self.assertEqual([c["function"]["name"] for c in calls], ["get_order", "get_customer"])
        self.assertEqual(len({c["id"] for c in calls}), 2)
        self.assertEqual(json.loads(calls[0]["function"]["arguments"]), {"order_id": "A-1001"})
        self.assertIn("usage", reply)

    def test_a_streamed_reply_adds_up_to_the_same_answer(self):
        raw = self.ask({"model": "mock-0", "stream": True, "stream_options": {"include_usage": True}, "messages": [{"role": "user", "content": "hello there"}]}).read().decode()
        events = [json.loads(line[6:]) for line in raw.splitlines() if line.startswith("data: ") and line != "data: [DONE]"]
        self.assertTrue(raw.rstrip().endswith("data: [DONE]"))
        text = "".join(e["choices"][0]["delta"].get("content", "") for e in events if e["choices"])
        self.assertEqual(text, "[simulated reply] You said: “hello there”.")
        self.assertEqual(events[-1]["usage"]["total_tokens"], 60)

    def test_it_waits_as_long_as_the_model_name_says(self):
        started = time.perf_counter()
        self.ask({"model": "mock-150", "messages": [{"role": "user", "content": "hi"}]}).read()
        self.assertGreaterEqual(time.perf_counter() - started, 0.14)


if __name__ == "__main__":
    unittest.main()
