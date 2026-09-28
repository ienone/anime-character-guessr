import importlib.util
import io
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch
import urllib.error

spec = importlib.util.spec_from_file_location("forensics", Path(__file__).with_name("anime-character-guessr-forensics.py"))
monitor = importlib.util.module_from_spec(spec)
spec.loader.exec_module(monitor)


class Response(io.BytesIO):
    status = 200


class ForensicsTests(unittest.TestCase):
    def test_three_failures_capture_once_then_rearm_after_recovery(self):
        state = {}
        for expected in (False, False, True):
            state, capture = monitor.transition(state, "42:100", False)
            self.assertEqual(capture, expected)
        state["captured"] = True
        state, capture = monitor.transition(state, "42:100", False)
        self.assertFalse(capture)
        state, _ = monitor.transition(state, "42:100", True)
        for _ in range(3):
            state, capture = monitor.transition(state, "42:100", False)
        self.assertTrue(capture)

    def test_pid_reuse_resets_failures(self):
        state, capture = monitor.transition({"identity": "42:100", "failures": 99, "captured": True}, "42:200", False)
        self.assertEqual(state["failures"], 1)
        self.assertFalse(capture)

    def test_health_rejects_spa_fallback(self):
        with patch.object(monitor.OPENER, "open", return_value=Response(b"<html>OK</html>")):
            self.assertFalse(monitor.probe("health", "/health")["ok"])

    def test_search_validates_shape_without_saving_results(self):
        with patch.object(monitor.OPENER, "open", return_value=Response(b'{"data":[{"id":1,"name":"hidden"}]}')):
            result = monitor.probe("search", "/search")
        self.assertTrue(result["ok"])
        self.assertEqual(result["results"], 1)
        self.assertNotIn("hidden", json.dumps(result))
        with patch.object(monitor.OPENER, "open", return_value=Response(b'[]')):
            self.assertFalse(monitor.probe("search", "/search")["ok"])

    def test_http_error_and_timeout_are_failures(self):
        for error in (TimeoutError(), urllib.error.HTTPError("local", 503, "busy", {}, None)):
            with self.subTest(error=type(error).__name__), patch.object(monitor.OPENER, "open", side_effect=error):
                result = monitor.probe("search", "/search")
                self.assertFalse(result["ok"])
                self.assertIn("error", result)

    def test_partial_search_failure_triggers_and_recent_history_is_bounded(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            samples = root / "recent.jsonl"
            samples.write_text('{}\n' * 120)
            def probe(name, _path):
                return {"ok": name != "search", "elapsed_ms": 0}
            with patch.object(monitor, "STATE", root), patch.object(monitor, "OUTPUT", root / "captures"), \
                 patch.object(monitor.subprocess, "check_output", return_value=b"42\n"), \
                 patch.object(monitor, "identity_for", return_value="42:100"), \
                 patch.object(monitor, "process_sample", return_value={}), \
                 patch.object(monitor, "probe", side_effect=probe), \
                 patch.object(monitor, "capture") as capture:
                for _ in range(4):
                    monitor.main()
                capture.assert_called_once()
                self.assertEqual(len(samples.read_text().splitlines()), 120)
                state = json.loads((root / "state.json").read_text())
                self.assertTrue(state["captured"])

    def test_unresponsive_health_skips_other_probes(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            with patch.object(monitor, "STATE", root), patch.object(monitor, "OUTPUT", root / "captures"), \
                 patch.object(monitor.subprocess, "check_output", return_value=b"42\n"), \
                 patch.object(monitor, "identity_for", return_value="42:100"), \
                 patch.object(monitor, "process_sample", return_value={}), \
                 patch.object(monitor, "probe", return_value={"ok": False}) as probe:
                monitor.main()
                self.assertEqual(probe.call_count, 1)


if __name__ == "__main__":
    unittest.main()
