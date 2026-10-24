"""The playground's controller, with the real workers (skipped when the
framework environments are not set up: `python benches/compare/setup.py`).

    target/compare/venvs/agentbuild/Scripts/python.exe -m unittest discover playground
"""

import json
import threading
import unittest
import urllib.error
import urllib.request
from http.server import ThreadingHTTPServer

import server

READY = all(server.python_of(f).exists() for f in server.FRAMEWORKS)


class Limits(unittest.TestCase):
    """What the page may ask for is checked and bounded."""

    @unittest.skipUnless(READY, "framework environments not set up")
    def test_values_are_clamped_and_defaults_apply(self):
        job = server.read_job({"message": "hi", "tools": 99, "runs": 10_000, "concurrency": 0, "model_delay_ms": -5, "order": "x"})
        self.assertEqual((job["tools"], job["runs"], job["concurrency"], job["model_delay_ms"]), (5, 200, 1, 0))
        self.assertEqual(job["order"], "parallel")
        self.assertEqual(job["frameworks"], ["agentbuild", "langchain"])
        self.assertEqual(len(server.read_job({"message": "x" * 5000})["message"]), 2000)
        self.assertEqual(server.read_job({"message": "hi", "frameworks": ["langchain", "nope"]})["frameworks"], ["langchain"])

    @unittest.skipUnless(READY, "framework environments not set up")
    def test_a_value_that_was_limited_is_reported(self):
        job = server.read_job({"message": "hi", "tools": 10, "concurrency": 1000, "runs": 30})
        self.assertEqual(job["limited"], {"tools": [10, 5], "concurrency": [1000, 100]})
        self.assertEqual(server.read_job({"message": "hi"})["limited"], {})
        self.assertEqual(server.read_job({"message": "x" * 2500})["limited"], {"message": [2500, 2000]})

    def test_bad_requests_are_refused(self):
        for body in [{}, {"message": "   "}, {"message": "hi", "frameworks": ["nope"]}]:
            with self.assertRaises(ValueError):
                server.read_job(body)


@unittest.skipUnless(READY, "framework environments not set up")
class Runs(unittest.TestCase):
    def test_both_frameworks_run_and_are_measured(self):
        job = server.read_job({"message": "Where is my refund?", "tools": 2, "model_delay_ms": 20, "tool_delay_ms": 20,
                               "runs": 5, "concurrency": 2, "order": "parallel"})
        events = []
        server.run_job(job, events.append)
        by = {(e.get("fw"), e["e"]): e for e in events}
        self.assertEqual(events[0]["e"], "job")
        self.assertEqual(events[-1]["e"], "end")
        answers = set()
        for fw in ("agentbuild", "langchain"):
            self.assertEqual(by[(fw, "exit")]["code"], 0, fw)
            self.assertGreater(by[(fw, "exit")]["peak_rss_mb"], 5, "the real interpreter was measured, not the launcher")
            self.assertGreater(by[(fw, "exit")]["cpu_time_s"], 0)
            self.assertEqual(len(by[(fw, "done")]["runs"]), 5)
            self.assertEqual(sum(1 for e in events if e.get("fw") == fw and e["e"] == "tool"), 2)
            answers.add(by[(fw, "first_run")]["answer"])
        self.assertEqual(len(answers), 1, "both frameworks answered the same")
        self.assertIn("You said: “Where is my refund?”", answers.pop())

    def test_one_after_the_other_runs_a_b_b_a_without_overlap(self):
        job = server.read_job({"message": "hi", "tools": 0, "model_delay_ms": 0, "tool_delay_ms": 0, "runs": 3, "order": "sequential"})
        events = []
        server.run_job(job, events.append)
        exits = [e for e in events if e["e"] == "exit"]
        self.assertEqual([(e["fw"], e["pass"]) for e in exits], [("agentbuild", 0), ("langchain", 0), ("langchain", 1), ("agentbuild", 1)])
        starts = [e for e in events if e["e"] == "start"]
        for before, after in zip(exits, starts[1:]):
            self.assertGreaterEqual(after["t"], before["t"], "the next one started after the previous finished")

    def test_one_pass_each_when_not_balanced(self):
        job = server.read_job({"message": "hi", "tools": 0, "model_delay_ms": 0, "tool_delay_ms": 0, "runs": 2, "order": "sequential", "balanced": False})
        events = []
        server.run_job(job, events.append)
        self.assertEqual([e["fw"] for e in events if e["e"] == "exit"], ["agentbuild", "langchain"])

    def test_the_fake_model_server_is_stopped_afterwards(self):
        import psutil

        job = server.read_job({"message": "hi", "tools": 0, "model_delay_ms": 0, "tool_delay_ms": 0, "runs": 1, "order": "parallel"})
        events = []
        server.run_job(job, events.append)
        leftover = [
            p for p in psutil.process_iter(["name", "cmdline"])
            if (p.info["name"] or "").lower().startswith("python") and "mock_model.py" in " ".join(p.info["cmdline"] or [])
        ]
        self.assertEqual(leftover, [])


class Http(unittest.TestCase):
    def setUp(self):
        self.http = ThreadingHTTPServer(("127.0.0.1", 0), server.Handler)
        threading.Thread(target=self.http.serve_forever, daemon=True).start()
        self.base = f"http://127.0.0.1:{self.http.server_address[1]}"

    def tearDown(self):
        self.http.shutdown()

    def post(self, body):
        request = urllib.request.Request(self.base + "/api/run", data=json.dumps(body).encode(), method="POST")
        return urllib.request.urlopen(request, timeout=60)

    def test_it_listens_on_this_computer_only(self):
        self.assertEqual(self.http.server_address[0], "127.0.0.1")

    def test_the_page_and_info_are_served(self):
        self.assertIn(b"Playground", urllib.request.urlopen(self.base + "/").read())
        info = json.load(urllib.request.urlopen(self.base + "/api/info"))
        self.assertIn("machine", info)
        self.assertEqual(set(info["frameworks"]), {"agentbuild", "langchain"})

    def test_a_bad_request_is_a_400_and_a_busy_server_a_409(self):
        with self.assertRaises(urllib.error.HTTPError) as caught:
            self.post({"message": ""})
        self.assertEqual(caught.exception.code, 400)
        self.assertTrue(server.RUN_LOCK.acquire(blocking=False))
        try:
            with self.assertRaises(urllib.error.HTTPError) as caught:
                self.post({"message": "hi"})
            self.assertEqual(caught.exception.code, 409)
        finally:
            server.RUN_LOCK.release()

    def test_another_sites_page_cannot_start_a_run(self):
        body = json.dumps({"message": "hi"}).encode()
        request = urllib.request.Request(self.base + "/api/run", data=body, method="POST", headers={"Origin": "https://evil.example"})
        with self.assertRaises(urllib.error.HTTPError) as caught:
            urllib.request.urlopen(request, timeout=10)
        self.assertEqual(caught.exception.code, 403)

    def test_the_website_may_ask_and_its_preflight_is_answered(self):
        preflight = urllib.request.Request(self.base + "/api/run", method="OPTIONS", headers={"Origin": "http://localhost:4400"})
        answer = urllib.request.urlopen(preflight, timeout=10)
        self.assertEqual(answer.status, 204)
        self.assertEqual(answer.headers["access-control-allow-origin"], "http://localhost:4400")
        self.assertEqual(answer.headers["access-control-allow-private-network"], "true")
        info = urllib.request.urlopen(urllib.request.Request(self.base + "/api/info", headers={"Origin": "http://localhost:4400"}))
        self.assertEqual(info.headers["access-control-allow-origin"], "http://localhost:4400")
        stranger = urllib.request.Request(self.base + "/api/run", method="OPTIONS", headers={"Origin": "https://evil.example"})
        with self.assertRaises(urllib.error.HTTPError) as caught:
            urllib.request.urlopen(stranger, timeout=10)
        self.assertEqual(caught.exception.code, 403)

    def test_its_own_page_is_allowed(self):
        request = urllib.request.Request(self.base + "/api/info", headers={"Origin": self.base})
        self.assertEqual(urllib.request.urlopen(request, timeout=10).status, 200)


if __name__ == "__main__":
    unittest.main()
