#!/usr/bin/env python3
"""Verify the exact native smoke driver's lifecycle with simulated HTTP only."""

from __future__ import annotations

from contextlib import redirect_stdout
import importlib.util
import io
import json
from pathlib import Path
import tempfile
import time
import unittest
from unittest.mock import patch
from urllib.error import URLError
from urllib.parse import urlsplit


SOURCE = Path(__file__).with_name("native-smoke.py")
SPEC = importlib.util.spec_from_file_location("native_smoke_driver_under_test", SOURCE)
assert SPEC is not None and SPEC.loader is not None
SMOKE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(SMOKE)
ASSISTANT_SOURCE = SOURCE.with_name("native-assistant-smoke.py")
ASSISTANT_SPEC = importlib.util.spec_from_file_location("native_assistant_driver_under_test", ASSISTANT_SOURCE)
assert ASSISTANT_SPEC is not None and ASSISTANT_SPEC.loader is not None
ASSISTANT = importlib.util.module_from_spec(ASSISTANT_SPEC)
ASSISTANT_SPEC.loader.exec_module(ASSISTANT)
SESSION = "synthetic-private-session"
PHASES = {
    "requestOperation", "persistenceRead", "persistenceSave", "driverReady",
    "sessionDelete", "sessionCreate", "timeouts", "bridge",
}


class Response(io.BytesIO):
    def __init__(self, value):
        super().__init__(json.dumps({"value": value}).encode())


class FakeHttp:
    def __init__(self, outcomes, before_response=None):
        self.outcomes = list(outcomes)
        self.calls = []
        self.before_response = before_response

    def open(self, request, timeout):
        self.calls.append((request.get_method(), urlsplit(request.full_url).path))
        if self.before_response:
            self.before_response(request, timeout)
        if not self.outcomes:
            raise AssertionError("Unexpected additional HTTP request")
        outcome = self.outcomes.pop(0)
        if isinstance(outcome, Exception):
            raise outcome
        return Response(outcome)


class NativeDriverTests(unittest.TestCase):
    def setUp(self):
        blocker = patch("urllib.request.OpenerDirector.open", side_effect=AssertionError("Real network forbidden"))
        blocker.start()
        self.addCleanup(blocker.stop)

    def driver(self, outcomes, session=SESSION, before_response=None):
        driver = SMOKE.Driver("http://127.0.0.1:4444", 60)
        driver.session_id = session
        driver.opener = FakeHttp(outcomes, before_response)
        return driver

    def test_delete_ack_clears_identifier_only_after_ack_and_close_becomes_noop(self):
        driver = self.driver([None])
        driver.opener.before_response = lambda request, timeout: self.assertEqual(driver.session_id, SESSION)
        deadline = driver.deadline
        driver.close()
        self.assertIsNone(driver.session_id)
        self.assertEqual(driver.deadline, deadline)
        driver.close()
        self.assertEqual(driver.opener.calls, [("DELETE", "/session/" + SESSION)])

    def test_delete_reset_retains_identifier_and_prevents_a_new_session(self):
        driver = self.driver([URLError("synthetic reset with private details")])
        with self.assertRaises(SMOKE.SmokeFailure) as captured:
            driver.close()
        self.assertEqual(str(captured.exception), "webdriverUnreachable")
        self.assertEqual(captured.exception.phase, "sessionDelete")
        self.assertEqual(driver.session_id, SESSION)
        with self.assertRaisesRegex(SMOKE.SmokeFailure, "webdriverSessionAlreadyOpen"):
            driver.start(Path("/synthetic/application"))
        self.assertEqual(driver.opener.calls, [("DELETE", "/session/" + SESSION)])

    def test_cleanup_retains_identifier_until_ack_and_restores_expired_deadline(self):
        driver = self.driver([URLError("synthetic reset"), None])
        expired = time.monotonic() - 1
        driver.deadline = expired
        driver.opener.before_response = lambda request, timeout: (
            self.assertEqual(driver.session_id, SESSION), self.assertGreater(timeout, 0),
            self.assertLessEqual(timeout, 10),
        )
        with self.assertRaises(SMOKE.SmokeFailure):
            driver.close()
        self.assertEqual(driver.session_id, SESSION)
        self.assertEqual(driver.deadline, expired)
        driver.close()
        self.assertIsNone(driver.session_id)
        self.assertEqual(driver.deadline, expired)
        self.assertEqual([method for method, _ in driver.opener.calls], ["DELETE", "DELETE"])

    def test_non_ack_delete_response_retains_identifier(self):
        driver = self.driver([{"unexpected": "response"}])
        with self.assertRaises(SMOKE.SmokeFailure):
            driver.close()
        self.assertEqual(driver.session_id, SESSION)
        self.assertEqual(len(driver.opener.calls), 1)

    def test_reset_during_session_creation_never_repeats_post(self):
        driver = self.driver([{"ready": True}, URLError("synthetic reset")], session=None)
        with self.assertRaises(SMOKE.SmokeFailure) as captured:
            driver.start(Path("/synthetic/application"))
        self.assertEqual(captured.exception.phase, "sessionCreate")
        self.assertIsNone(driver.session_id)
        self.assertEqual(driver.opener.calls, [("GET", "/status"), ("POST", "/session")])

    def test_specific_phase_overrides_generic_transport_phase_without_leaking_payload(self):
        driver = self.driver([URLError("synthetic reset with private details")])
        report = {"checks": []}
        with self.assertRaises(SMOKE.SmokeFailure):
            with SMOKE.checked(report, "syntheticPersistence"):
                with SMOKE.diagnostic_phase("persistenceSave"):
                    driver.invoke("settings_save", {"privatePayload": "synthetic-sensitive-value"})
        self.assertEqual(report["checks"][0]["status"], "failed")
        self.assertEqual(report["checks"][0]["phase"], "persistenceSave")
        encoded = json.dumps(report)
        for forbidden in (SESSION, "127.0.0.1", "http", "privatePayload", "synthetic-sensitive-value", "private details"):
            self.assertNotIn(forbidden, encoded)

    def test_failure_phase_accepts_only_fixed_allowlist(self):
        for phase in PHASES:
            with self.subTest(phase=phase):
                self.assertEqual(SMOKE.SmokeFailure("syntheticFailure", phase).phase, phase)
        for phase in (None, "unknown", SESSION, "https://private.invalid/value"):
            with self.subTest(phase=phase):
                self.assertIsNone(SMOKE.SmokeFailure("syntheticFailure", phase).phase)

    def test_failed_check_suppresses_a_mutated_unknown_phase(self):
        error = SMOKE.SmokeFailure("syntheticFailure")
        error.phase = "https://private.invalid/" + SESSION
        report = {"checks": []}
        with self.assertRaises(SMOKE.SmokeFailure):
            with SMOKE.checked(report, "syntheticFailure"):
                raise error
        self.assertEqual(report["checks"][0]["status"], "failed")
        self.assertNotIn("phase", report["checks"][0])
        self.assertNotIn(SESSION, json.dumps(report))

    def test_genuine_failed_check_remains_failed_with_safe_phase_in_main_report(self):
        def fail(driver, binary, profile, screenshot, report):
            with SMOKE.checked(report, "syntheticAssertion"):
                with SMOKE.diagnostic_phase("bridge"):
                    SMOKE.require(False, "syntheticAssertionFailed")

        with tempfile.TemporaryDirectory(prefix="native-driver-tests-") as temporary:
            root = Path(temporary)
            binary = root / "synthetic-binary"
            binary.write_bytes(b"synthetic executable fixture")
            binary.chmod(0o700)
            output = io.StringIO()
            driver = self.driver([], session=None)
            args = [str(SOURCE), "--binary", str(binary), "--profile-root", str(root / "fresh-profile")]
            with patch("sys.argv", args), patch.object(SMOKE, "Driver", return_value=driver), patch.object(SMOKE, "smoke", fail), redirect_stdout(output):
                result = SMOKE.main()
            report = json.loads(output.getvalue())
        self.assertEqual(result, 1)
        self.assertEqual(report["status"], "failed")
        self.assertEqual(report["errorCode"], "syntheticAssertionFailed")
        self.assertEqual(report["failurePhase"], "bridge")
        self.assertEqual(report["checks"][0]["status"], "failed")
        self.assertEqual(report["checks"][0]["phase"], "bridge")
        self.assertEqual(driver.opener.calls, [])

    def test_final_cleanup_reset_fails_success_and_preserves_an_existing_failure(self):
        for scenario in ("general", "assistant", "catalogue"):
            module = SMOKE if scenario == "general" else ASSISTANT
            native = SMOKE if scenario == "general" else ASSISTANT.native
            callback = "catalogue_actions_smoke" if scenario == "catalogue" else "smoke"
            for previous_failure in (False, True):
                with self.subTest(scenario=scenario, previous_failure=previous_failure):
                    def checks(*arguments):
                        report = arguments[-1]
                        report["checks"] = [
                            {"name": "syntheticCheck" + str(index), "status": "passed", "durationMs": 1}
                            for index in range(10)
                        ]
                        if previous_failure:
                            with native.checked(report, "syntheticAssertion"):
                                with native.diagnostic_phase("bridge"):
                                    native.require(False, "syntheticAssertionFailed")

                    with tempfile.TemporaryDirectory(prefix="native-driver-cleanup-tests-") as temporary:
                        root = Path(temporary)
                        binary = root / "synthetic-binary"
                        binary.write_bytes(b"synthetic executable fixture")
                        binary.chmod(0o700)
                        report_path = root / "report.json"
                        driver = native.Driver("http://127.0.0.1:4444", 60)
                        driver.session_id = SESSION
                        driver.opener = FakeHttp([URLError("synthetic reset with private details")])
                        args = [str(SOURCE), "--binary", str(binary), "--profile-root", str(root / "fresh-profile"),
                                "--report", str(report_path)]
                        if scenario == "catalogue":
                            args.append("--catalogue-actions")
                        output = io.StringIO()
                        with patch("sys.argv", args), patch.object(module, "Driver", return_value=driver), \
                                patch.object(module, callback, checks), redirect_stdout(output):
                            result = module.main()
                        report = json.loads(output.getvalue())
                        self.assertEqual(json.loads(report_path.read_text(encoding="utf-8")), report)
                    self.assertEqual(result, 1)
                    self.assertEqual(report["status"], "failed")
                    self.assertEqual(report["cleanupWarning"], "webdriverSessionCloseFailed")
                    self.assertEqual(report["errorCode"], "syntheticAssertionFailed" if previous_failure
                                     else "webdriverSessionCloseFailed")
                    self.assertEqual(report["failurePhase"], "bridge" if previous_failure else "sessionDelete")
                    self.assertEqual(sum(check["status"] == "passed" for check in report["checks"]), 10)
                    self.assertEqual(driver.session_id, SESSION)
                    self.assertEqual(driver.opener.calls, [("DELETE", "/session/" + SESSION)])
                    for forbidden in (SESSION, "127.0.0.1", "private details"):
                        self.assertNotIn(forbidden, json.dumps(report))


if __name__ == "__main__":
    unittest.main(verbosity=2)
