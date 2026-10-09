#!/usr/bin/env python3
"""Exercise the packaged application's real IPC in an isolated WebDriver session."""

from __future__ import annotations

import argparse
import base64
from contextlib import contextmanager
import hashlib
from html.parser import HTMLParser
import json
import os
from pathlib import Path
import tempfile
import time
import unittest
from urllib.error import HTTPError, URLError
from urllib.parse import quote, urlsplit
from urllib.request import ProxyHandler, Request, build_opener


MAX_RESPONSE_BYTES = 16 * 1024 * 1024
MAX_FIXTURE_BYTES = 8 * 1024 * 1024
PROVIDER_IDS = {"zai", "kimi", "minimax", "codex", "claude", "mistral"}
TERMINAL_JOBS = {"completed", "failed", "cancelled"}
FIXTURE_TEXT = (
    "Bibliothèque synthétique de validation.\n\n"
    "SMOKE_NATIVE_PARAGRAPH_A vérifie les accents : été, cœur, bibliothèque.\n\n"
    "SMOKE_NATIVE_PARAGRAPH_B vérifie que la conversion conserve le contenu.\n"
)
QUERY = {
    "search": "", "authors": [], "series": [], "genres": [], "tags": [],
    "languages": [], "formats": [], "deviceId": None, "onDevice": None,
    "readStatus": None, "favorite": None, "metadataStatus": None,
    "missingCover": None, "minSizeBytes": None, "maxSizeBytes": None,
    "sort": "title", "descending": False, "offset": 0, "limit": 48,
}
INVOKE_SCRIPT = """
const command = arguments[0], parameters = arguments[1];
const finish = arguments[arguments.length - 1];
const bridge = window.__TAURI_INTERNALS__;
if (!bridge || typeof bridge.invoke !== 'function') {
  finish({ok:false,error:{code:'bridgeUnavailable',retryable:false}});
} else {
  Promise.resolve().then(() => bridge.invoke(command, parameters)).then(
    value => finish({ok:true,value}),
    error => finish({ok:false,error:{
      code:typeof error?.code === 'string' ? error.code : 'unstructuredIpcError',
      retryable:error?.retryable === true
    }})
  );
}
"""


class SmokeFailure(RuntimeError):
    """Only fixed diagnostic codes are printed; raw IPC/driver details stay private."""


class IpcFailure(SmokeFailure):
    def __init__(self, code: str):
        self.code = code if code.isidentifier() and len(code) <= 64 else "invalidErrorCode"
        super().__init__(self.code)


def require(condition: bool, code: str) -> None:
    if not condition:
        raise SmokeFailure(code)


def file_sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        while block := source.read(128 * 1024):
            digest.update(block)
    return digest.hexdigest()


def isolated_profile(value: str) -> Path:
    path = Path(value)
    require(path.is_absolute() and not path.is_symlink(), "isolatedProfileMustBeAbsolute")
    require(".." not in path.parts and not any(ord(char) < 32 for char in value), "unsafeProfilePath")
    root = path.resolve()
    home = Path.home().resolve()
    require(root not in {Path("/"), Path("/tmp"), home, home / ".local", home / ".local/share"}, "profileRootTooBroad")
    if root.exists():
        require(root.is_dir() and not any(root.iterdir()), "profileMustBeFreshAndEmpty")
    else:
        root.mkdir(parents=True, mode=0o700)
    return root


class PlainText(HTMLParser):
    def __init__(self) -> None:
        super().__init__(convert_charrefs=True)
        self.fragments: list[str] = []
        self.ignored = 0

    def handle_starttag(self, tag: str, attrs: list) -> None:
        if tag in {"script", "style"}:
            self.ignored += 1

    def handle_endtag(self, tag: str) -> None:
        if tag in {"script", "style"}:
            self.ignored = max(0, self.ignored - 1)

    def handle_data(self, data: str) -> None:
        if not self.ignored:
            self.fragments.append(data)


def section_text(section: dict) -> str:
    html = section.get("html")
    require(isinstance(html, str), "readerHtmlMissing")
    parser = PlainText()
    parser.feed(html)
    return " ".join(" ".join(parser.fragments).split())


def mobi_roundtrip_fixture(source: Path, destination: Path) -> None:
    """Give our generated PalmDB a distinct database label, preserving all records.

    The managed conversion already owns its hash. A different synthetic PalmDB
    label permits an independent MOBI import to exercise the bundled input engine,
    rather than incorrectly calling EPUB -> EPUB a MOBI reconstruction test.
    """
    require(source.is_file() and not source.is_symlink(), "generatedMobiMissing")
    require(source.stat().st_size <= MAX_FIXTURE_BYTES, "generatedMobiTooLarge")
    data = bytearray(source.read_bytes())
    require(len(data) >= 78 and data[60:68] == b"BOOKMOBI", "generatedMobiPalmHeaderInvalid")
    data[:32] = b"LM smoke MOBI round trip".ljust(32, b"\0")
    destination.write_bytes(data)
    require(file_sha256(source) != file_sha256(destination), "roundTripFixtureNotDistinct")


class Driver:
    def __init__(self, url: str, timeout: float) -> None:
        parsed = urlsplit(url)
        require(parsed.scheme == "http" and parsed.hostname in {"localhost", "127.0.0.1", "::1"}, "driverMustBeLocalHttp")
        require(not parsed.username and not parsed.password and parsed.path in {"", "/"} and not parsed.query and not parsed.fragment, "driverUrlInvalid")
        self.url = url.rstrip("/")
        self.session_id: str | None = None
        self.deadline = time.monotonic() + timeout
        self.opener = build_opener(ProxyHandler({}))

    def remaining(self) -> float:
        remaining = self.deadline - time.monotonic()
        require(remaining > 0, "smokeDeadlineExceeded")
        return remaining

    def request(self, method: str, path: str, payload: dict | None = None):
        data = json.dumps(payload).encode() if payload is not None else None
        request = Request(self.url + path, data=data, method=method, headers={"Content-Type": "application/json"})
        try:
            with self.opener.open(request, timeout=min(35, self.remaining())) as response:
                content = response.read(MAX_RESPONSE_BYTES + 1)
        except HTTPError as error:
            # WebDriver's message can contain paths, JS arguments or source text.
            code = f"webdriverHttp{error.code}"
            error.close()
            raise SmokeFailure(code) from None
        except (URLError, TimeoutError, OSError):
            raise SmokeFailure("webdriverUnreachable") from None
        require(len(content) <= MAX_RESPONSE_BYTES, "webdriverResponseTooLarge")
        try:
            response = json.loads(content)
        except (UnicodeError, json.JSONDecodeError):
            raise SmokeFailure("webdriverResponseInvalid") from None
        require(isinstance(response, dict) and "value" in response, "webdriverResponseShapeInvalid")
        value = response["value"]
        if isinstance(value, dict) and value.get("error"):
            raise SmokeFailure("webdriverCommandRejected")
        return value

    def endpoint(self, suffix: str) -> str:
        require(self.session_id is not None, "webdriverSessionMissing")
        return "/session/" + quote(self.session_id, safe="") + suffix

    def start(self, binary: Path) -> None:
        require(self.session_id is None, "webdriverSessionAlreadyOpen")
        def ready():
            try:
                status = self.request("GET", "/status")
            except SmokeFailure as error:
                if str(error) in {"webdriverUnreachable", "webdriverHttp500", "webdriverHttp503"}:
                    return False
                raise
            return isinstance(status, dict) and status.get("ready") is True
        self.wait(ready, "webdriverNotReady", 15)
        response = self.request("POST", "/session", {"capabilities": {"alwaysMatch": {"tauri:options": {"application": str(binary), "args": []}}, "firstMatch": [{}]}})
        require(isinstance(response, dict) and isinstance(response.get("sessionId"), str), "webdriverSessionShapeInvalid")
        session_id = response["sessionId"]
        require(0 < len(session_id) <= 128, "webdriverSessionIdInvalid")
        self.session_id = session_id
        self.request("POST", self.endpoint("/timeouts"), {"script": 30000, "pageLoad": 30000, "implicit": 0})
        self.wait(lambda: self.execute("return typeof window.__TAURI_INTERNALS__?.invoke === 'function';"), "nativeBridgeNotReady")

    def close(self) -> None:
        if self.session_id is None:
            return
        endpoint = self.endpoint("")
        self.session_id = None
        # Cleanup must still run if the main smoke deadline expired.
        previous_deadline = self.deadline
        self.deadline = max(previous_deadline, time.monotonic() + 10)
        try:
            self.request("DELETE", endpoint)
        finally:
            self.deadline = previous_deadline

    def execute(self, script: str, arguments: list | None = None):
        return self.request("POST", self.endpoint("/execute/sync"), {"script": script, "args": arguments or []})

    def invoke(self, command: str, parameters: dict | None = None):
        response = self.request("POST", self.endpoint("/execute/async"), {"script": INVOKE_SCRIPT, "args": [command, parameters or {}]})
        require(isinstance(response, dict) and isinstance(response.get("ok"), bool), "ipcResponseShapeInvalid")
        if not response["ok"]:
            error = response.get("error")
            raise IpcFailure(error.get("code", "unstructuredIpcError") if isinstance(error, dict) else "unstructuredIpcError")
        require("value" in response, "ipcValueMissing")
        return response["value"]

    def wait(self, predicate, failure: str, seconds: float = 60):
        deadline = min(self.deadline, time.monotonic() + seconds)
        while time.monotonic() < deadline:
            value = predicate()
            if value:
                return value
            time.sleep(min(0.3, max(0, deadline - time.monotonic())))
        raise SmokeFailure(failure)

    def job(self, job: dict) -> dict:
        require(isinstance(job, dict) and isinstance(job.get("id"), str), "jobCreationInvalid")
        job_id = job["id"]

        def terminal():
            matches = [value for value in self.invoke("jobs_list") if value.get("id") == job_id]
            require(len(matches) == 1, "durableJobMissing")
            current = matches[0]
            if current.get("status") in TERMINAL_JOBS:
                if current["status"] != "completed":
                    error = current.get("error") or {}
                    raise IpcFailure(error.get("code", "jobDidNotComplete"))
                return current
            return None

        return self.wait(terminal, "jobTimedOut")


def prepare_reader_epub(driver: Driver, book_id: str, source_format: str) -> dict:
    """Non-EPUB imports remain originals until an explicit conversion completes."""
    completed = driver.job(driver.invoke("book_convert", {"id": book_id, "format": "epub"}))
    result = completed.get("result")
    require(isinstance(result, dict), "epubConversionReportMissing")
    require(result.get("sourceFormat") == source_format and result.get("targetFormat") == "epub" and result.get("afterBytes", 0) > 0, "explicitEpubConversionFailed")
    variants = driver.invoke("book_files", {"id": book_id})
    require(any(file["format"] == "epub" and file["bookId"] == book_id for file in variants), "explicitEpubVariantMissing")
    return result


@contextmanager
def checked(report: dict, name: str):
    start = time.monotonic()
    result = {"name": name, "status": "running"}
    report["checks"].append(result)
    try:
        yield
        result["status"] = "passed"
    except Exception:
        result["status"] = "failed"
        raise
    finally:
        result["durationMs"] = round((time.monotonic() - start) * 1000)


def smoke(driver: Driver, binary: Path, profile: Path, screenshot: Path | None, report: dict) -> None:
    with tempfile.TemporaryDirectory(prefix="library-manager-native-fixture-") as temporary:
        fixture = Path(temporary) / "Library Manager smoke fixture.txt"
        fixture.write_text(FIXTURE_TEXT, encoding="utf-8")
        fixture_hash = file_sha256(fixture)
        with checked(report, "packagedNativeWindowAndBootstrap"):
            driver.start(binary)
            bootstrap = driver.invoke("app_bootstrap")
            require(isinstance(bootstrap.get("version"), str), "bootstrapVersionMissing")
            settings = driver.invoke("settings_get")
            require(settings == bootstrap["settings"], "bootstrapSettingsDiverged")
            library_root = Path(settings["libraryRoot"]).resolve()
            require(library_root.is_relative_to(profile) and library_root != profile, "applicationDidNotUseIsolatedProfile")
            require(driver.invoke("library_list", {"query": QUERY})["total"] == 0, "libraryWasNotEmpty")
            native_dom = driver.wait(lambda: driver.execute("const text=document.body?.innerText?.length || 0; if (!document.querySelector('main') || text <= 80) return false; return {title:document.title,text,main:true,demo:new URL(location.href).searchParams.has('demo')};"), "nativeWindowEmpty", 20)
            require(native_dom["main"] and native_dom["text"] > 80 and "Library Manager" in native_dom["title"] and not native_dom["demo"], "nativeWindowIsBlankOrDemo")
            report["version"] = bootstrap["version"]

        with checked(report, "sixRealProviderAdaptersAndSettings"):
            providers = driver.invoke("providers_list")
            require(len(providers) == 6 and {provider["id"] for provider in providers} == PROVIDER_IDS, "providerAdapterSetInvalid")
            require(all(provider["connectionMode"] == "api" for provider in providers), "providerRequiresExternalCli")
            settings.update({"language": "fr", "autoEnrich": True, "webEnabled": False, "providerId": None, "modelId": None, "maxConcurrentJobs": 1})
            settings = driver.invoke("settings_save", {"settings": settings})
            require(settings["language"] == "fr" and not settings["webEnabled"], "safeTestSettingsNotSaved")

        with checked(report, "realImportAndHashDeduplication"):
            imported = driver.job(driver.invoke("import_books", {"paths": [str(fixture)]}))
            require(imported["result"]["imported"] == 1, "syntheticImportDidNotCreateBook")
            page = driver.invoke("library_list", {"query": QUERY})
            require(page["total"] == 1 and len(page["items"]) == 1, "libraryImportCountInvalid")
            book = page["items"][0]
            book_id = book["id"]
            duplicate = driver.job(driver.invoke("import_books", {"paths": [str(fixture)]}))
            require(duplicate["result"]["duplicates"] == 1 and duplicate["result"]["imported"] == 0, "hashDeduplicationFailed")
            require(driver.invoke("library_list", {"query": QUERY})["total"] == 1, "duplicateCreatedSecondBook")
            files = driver.invoke("book_files", {"id": book_id})
            require(any(file["variant"] == "original" and file["sha256"] == fixture_hash for file in files), "originalHashNotPreserved")

        with checked(report, "explicitTxtToEpubAndReaderContentAndSavedProgress"):
            converted = prepare_reader_epub(driver, book_id, "txt")
            report["readerPreparationSourceFormat"] = converted["sourceFormat"]
            manifest = driver.invoke("reader_open", {"id": book_id})
            require(manifest["bookId"] == book_id and len(manifest["sections"]) > 0, "readerSpineMissing")
            section = driver.invoke("reader_section", {"id": book_id, "sectionIndex": 0})
            text = section_text(section)
            require("SMOKE_NATIVE_PARAGRAPH_A" in text and "SMOKE_NATIVE_PARAGRAPH_B" in text and "cœur" in text, "readerLostSyntheticText")
            before_progress = driver.invoke("book_get", {"id": book_id})
            driver.invoke("reader_save_progress", {"id": book_id, "location": "section:0", "progress": 0.25})
            progress = driver.invoke("book_get", {"id": book_id})
            require(progress["readingProgress"] == 0.25 and progress["readStatus"] == "reading" and progress["revision"] > before_progress["revision"], "readerProgressNotSaved")

        with checked(report, "xteinkOptimizationPreservesTextAndSource"):
            profiles = driver.invoke("optimization_profiles")
            require(any(profile["id"] == "xteink" for profile in profiles), "xteinkProfileMissing")
            optimized = driver.job(driver.invoke("book_optimize", {"id": book_id, "profileId": "xteink"}))["result"]
            require(optimized["textPreserved"] and optimized["chaptersBefore"] == optimized["chaptersAfter"] > 0, "optimizationChangedTextOrSpine")
            require(file_sha256(fixture) == fixture_hash, "optimizationChangedImportSource")
            after = section_text(driver.invoke("reader_section", {"id": book_id, "sectionIndex": 0}))
            require("SMOKE_NATIVE_PARAGRAPH_A" in after and "SMOKE_NATIVE_PARAGRAPH_B" in after, "optimizedReaderLostText")

        with checked(report, "nativeMobiOutputAndExplicitBundledMobiToEpub"):
            capabilities = driver.invoke("conversion_capabilities")
            require({"epub", "mobi"}.issubset(capabilities["inputs"]) and {"epub", "mobi"}.issubset(capabilities["outputs"]), "bundledConversionEngineUnavailable")
            conversion = driver.job(driver.invoke("book_convert", {"id": book_id, "format": "mobi"}))["result"]
            require(conversion["sourceFormat"] in {"txt", "epub"} and conversion["targetFormat"] == "mobi" and conversion["afterBytes"] > 0, "nativeMobiConversionFailed")
            report["mobiOutputSourceFormat"] = conversion["sourceFormat"]
            mobi_files = [path for path in library_root.rglob("*.mobi") if path.is_file() and not path.is_symlink()]
            require(len(mobi_files) == 1, "generatedMobiVariantNotUnique")
            roundtrip = Path(temporary) / "MOBI input smoke fixture.mobi"
            mobi_roundtrip_fixture(mobi_files[0], roundtrip)
            reconstructed = driver.job(driver.invoke("import_books", {"paths": [str(roundtrip)]}))
            require(reconstructed["result"]["imported"] == 1, "mobiInputImportFailed")
            second_ids = reconstructed["result"]["bookIds"]
            require(len(second_ids) == 1 and second_ids[0] != book_id, "mobiInputNotIndependentlyReconstructed")
            mobi_book_id = second_ids[0]
            reconstructed_epub = prepare_reader_epub(driver, mobi_book_id, "mobi")
            report["mobiReconstructionSourceFormat"] = reconstructed_epub["sourceFormat"]
            mobi_manifest = driver.invoke("reader_open", {"id": mobi_book_id})
            require(len(mobi_manifest["sections"]) > 0, "mobiReconstructionSpineMissing")
            mobi_text = section_text(driver.invoke("reader_section", {"id": mobi_book_id, "sectionIndex": 0}))
            require("SMOKE_NATIVE_PARAGRAPH_A" in mobi_text and "SMOKE_NATIVE_PARAGRAPH_B" in mobi_text, "mobiReconstructionLostText")
            variants = driver.invoke("book_files", {"id": mobi_book_id})
            require(any(file["format"] == "mobi" and file["variant"] == "original" for file in variants) and any(file["format"] == "epub" for file in variants), "mobiInputDidNotProduceManagedEpub")

        with checked(report, "metadataRevisionCompareAndSwapAndUndo"):
            before = driver.invoke("book_get", {"id": book_id})
            existing_operations = {operation["id"] for operation in driver.invoke("operations_list")}
            patch = {"title": "Library Manager — édition synthétique", "authors": ["Auteur Synthétique"], "authorSort": "Synthétique, Auteur", "series": "Série de validation", "seriesIndex": 0.5, "tags": ["native-smoke"], "genres": ["technology"]}
            edited = driver.invoke("book_update", {"id": book_id, "patch": patch, "expectedRevision": before["revision"]})
            require(edited["seriesIndex"] == 0.5 and edited["revision"] > before["revision"], "metadataRevisionNotChanged")
            try:
                driver.invoke("book_update", {"id": book_id, "patch": {"title": "Stale synthetic edit"}, "expectedRevision": before["revision"]})
            except IpcFailure as error:
                require(error.code == "revisionConflict", "revisionConflictCodeInvalid")
            else:
                raise SmokeFailure("staleMetadataRevisionAccepted")
            require(driver.invoke("book_get", {"id": book_id})["title"] == edited["title"], "staleMetadataEditChangedBook")
            operations = [operation for operation in driver.invoke("operations_list") if operation["id"] not in existing_operations and operation["reversible"]]
            require(len(operations) == 1, "metadataUndoOperationMissing")
            undone = driver.invoke("operation_undo", {"id": operations[0]["id"]})
            require(undone["status"] == "reverted", "metadataUndoDidNotComplete")
            restored = driver.invoke("book_get", {"id": book_id})
            require(restored["title"] == before["title"] and restored["series"] == before["series"] and restored["readingProgress"] == 0.25, "metadataUndoChangedUnrelatedState")

        with checked(report, "automaticAiEnrichmentWaitsForConfigurationWithoutNetwork"):
            def waiting():
                jobs = driver.invoke("jobs_list")
                return [job for job in jobs if job["kind"] == "enrich" and book_id in job["bookIds"] and job["status"] == "waitingForConfiguration"]
            waiting_jobs = driver.wait(waiting, "autoEnrichmentDidNotWaitForConfiguration", 25)
            require(waiting_jobs[0]["error"]["code"] == "providerNotConfigured", "configurationWaitCodeInvalid")
            for job in driver.invoke("jobs_list"):
                if job["kind"] == "enrich" and job["status"] not in TERMINAL_JOBS:
                    cancelled = driver.invoke("job_cancel", {"id": job["id"]})
                    require(cancelled["status"] == "cancelled", "waitingJobCancellationFailed")

        with checked(report, "languagePersistsAcrossNativeProcessRestart"):
            settings = driver.invoke("settings_get")
            require(settings["language"] == "fr", "frenchSettingNotPersisted")
            settings["language"] = "en"
            driver.invoke("settings_save", {"settings": settings})
            driver.close()
            driver.start(binary)
            bootstrap = driver.invoke("app_bootstrap")
            require(bootstrap["settings"]["language"] == "en", "englishSettingLostAcrossRestart")
            require(driver.invoke("library_list", {"query": QUERY})["total"] == 2, "libraryLostAcrossRestart")
            require(driver.invoke("book_get", {"id": book_id})["readingProgress"] == 0.25, "progressLostAcrossRestart")
            require(file_sha256(fixture) == fixture_hash, "nativeWorkflowModifiedSource")
            report["bookCount"] = 2
            report["providerCount"] = 6

        with checked(report, "nativeRenderedLibrary"):
            title = driver.invoke("book_get", {"id": book_id})["title"]
            driver.wait(lambda: driver.execute("return document.body?.innerText?.includes(arguments[0]) === true;", [title]), "nativeLibraryDidNotRenderImportedBook", 20)
            if screenshot:
                encoded = driver.request("GET", driver.endpoint("/screenshot"))
                require(isinstance(encoded, str), "nativeScreenshotResponseInvalid")
                try:
                    image = base64.b64decode(encoded, validate=True)
                except (ValueError, base64.binascii.Error):
                    raise SmokeFailure("nativeScreenshotBase64Invalid") from None
                require(image.startswith(b"\x89PNG\r\n\x1a\n"), "nativeScreenshotNotPng")
                screenshot.parent.mkdir(parents=True, exist_ok=True)
                screenshot.write_bytes(image)
                report["screenshotSaved"] = True


class SmokeTests(unittest.TestCase):
    def test_reader_preparation_waits_for_completed_conversion_before_using_variant(self) -> None:
        for source_format in ("txt", "mobi"):
            driver = Driver("http://localhost:4444", 3)
            calls = []
            pending = [{"id": "conversion", "status": "running"}, {"id": "conversion", "status": "completed", "result": {"sourceFormat": source_format, "targetFormat": "epub", "afterBytes": 1024}}]

            def invoke(command, parameters=None):
                calls.append((command, parameters))
                if command == "book_convert":
                    return {"id": "conversion", "status": "queued"}
                if command == "jobs_list":
                    return [pending.pop(0)]
                if command == "book_files":
                    self.assertFalse(pending, "The EPUB variant must not be used before the conversion finishes")
                    return [{"bookId": "book", "format": source_format}, {"bookId": "book", "format": "epub"}]
                self.fail("Unexpected command in reader preparation")

            driver.invoke = invoke
            result = prepare_reader_epub(driver, "book", source_format)
            self.assertEqual(result["sourceFormat"], source_format)
            self.assertEqual(calls[0], ("book_convert", {"id": "book", "format": "epub"}))
            self.assertEqual([command for command, _parameters in calls], ["book_convert", "jobs_list", "jobs_list", "book_files"])

    def test_reader_preparation_refuses_wrong_report_and_missing_epub_variant(self) -> None:
        driver = Driver("http://localhost:4444", 3)
        driver.job = lambda job: {"status": "completed", "result": {"sourceFormat": "txt", "targetFormat": "mobi", "afterBytes": 1024}}
        driver.invoke = lambda *args: {"id": "conversion"}
        with self.assertRaises(SmokeFailure):
            prepare_reader_epub(driver, "book", "txt")
        driver.job = lambda job: {"status": "completed", "result": {"sourceFormat": "mobi", "targetFormat": "epub", "afterBytes": 1024}}
        driver.invoke = lambda command, parameters: [] if command == "book_files" else {"id": "conversion"}
        with self.assertRaises(SmokeFailure):
            prepare_reader_epub(driver, "book", "mobi")

    def test_profile_guard_rejects_existing_data_and_generic_home(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary) / "fresh-profile"
            self.assertEqual(isolated_profile(str(root)), root)
            (root / "private-library.sqlite3").write_text("private")
            with self.assertRaises(SmokeFailure):
                isolated_profile(str(root))
        with self.assertRaises(SmokeFailure):
            isolated_profile(str(Path.home()))

    def test_async_ipc_rejections_never_turn_into_fake_success_or_print_raw_details(self) -> None:
        driver = Driver("http://127.0.0.1:4444", 3)
        driver.session_id = "synthetic-session"
        driver.request = lambda *args: {"ok": False, "error": {"code": "profileInUse", "message": "private path and token"}}
        with self.assertRaises(IpcFailure) as captured:
            driver.invoke("app_bootstrap")
        self.assertEqual(str(captured.exception), "profileInUse")
        self.assertNotIn("private", str(captured.exception))
        driver.request = lambda *args: {"ok": True}
        with self.assertRaises(SmokeFailure):
            driver.invoke("library_list")

    def test_failed_or_cancelled_jobs_cannot_be_reported_as_completed(self) -> None:
        driver = Driver("http://localhost:4444", 3)
        for status in ("failed", "cancelled"):
            driver.invoke = lambda *args, status=status: [{"id": "job", "status": status, "error": {"code": "cancelled"}}]
            with self.assertRaises(IpcFailure):
                driver.job({"id": "job"})
        driver.invoke = lambda *args: [{"id": "job", "status": "completed", "result": {"imported": 1}}]
        self.assertEqual(driver.job({"id": "job"})["result"]["imported"], 1)

    def test_mobi_fixture_only_changes_synthetic_database_name_and_keeps_source(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            source = Path(temporary) / "generated.mobi"
            destination = Path(temporary) / "roundtrip.mobi"
            data = bytearray(256)
            data[60:68] = b"BOOKMOBI"
            data[78:] = b"X" * (256 - 78)
            source.write_bytes(data)
            before = file_sha256(source)
            mobi_roundtrip_fixture(source, destination)
            self.assertEqual(file_sha256(source), before)
            self.assertEqual(destination.read_bytes()[32:], source.read_bytes()[32:])
            source.write_bytes(b"not a MOBI")
            with self.assertRaises(SmokeFailure):
                mobi_roundtrip_fixture(source, destination)

    def test_deadline_local_url_and_content_validation(self) -> None:
        for url in ("https://example.org", "http://user:password@localhost", "http://127.0.0.1/private?token=1"):
            with self.assertRaises(SmokeFailure):
                Driver(url, 1)
        driver = Driver("http://127.0.0.1:4444", 1)
        driver.deadline = time.monotonic() - 1
        with self.assertRaises(SmokeFailure):
            driver.remaining()
        self.assertEqual(section_text({"html": "<style>private CSS</style><p>cœur &amp; été</p>"}), "cœur & été")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--driver-url", default="http://127.0.0.1:4444")
    parser.add_argument("--binary", type=Path)
    parser.add_argument("--profile-root", default=os.environ.get("XDG_DATA_HOME"), help="Fresh isolated XDG_DATA_HOME inherited by tauri-driver")
    parser.add_argument("--screenshot", type=Path)
    parser.add_argument("--report", type=Path)
    parser.add_argument("--timeout", type=float, default=240)
    parser.add_argument("--self-test", action="store_true")
    arguments = parser.parse_args()
    if arguments.self_test:
        result = unittest.TextTestRunner(verbosity=2).run(unittest.defaultTestLoader.loadTestsFromTestCase(SmokeTests))
        return 0 if result.wasSuccessful() else 1
    if not arguments.binary or not arguments.profile_root:
        parser.error("--binary and an isolated --profile-root or XDG_DATA_HOME are required")
    report = {"schemaVersion": 1, "status": "running", "checks": [], "paidApiCalls": 0, "physicalDeviceWrites": 0, "screenshotSaved": False}
    driver = None
    exit_code = 1
    try:
        require(30 <= arguments.timeout <= 600, "smokeTimeoutOutOfRange")
        binary = arguments.binary.resolve(strict=True)
        require(binary.is_file() and os.access(binary, os.X_OK), "packagedApplicationNotExecutable")
        profile = isolated_profile(arguments.profile_root)
        report["binaryName"] = binary.name
        report["binarySha256"] = file_sha256(binary)
        driver = Driver(arguments.driver_url, arguments.timeout)
        smoke(driver, binary, profile, arguments.screenshot, report)
        report["status"] = "passed"
        exit_code = 0
    except (SmokeFailure, OSError, ValueError, KeyError, TypeError) as error:
        report["status"] = "failed"
        report["errorCode"] = str(error) if isinstance(error, SmokeFailure) else "smokeInfrastructureOrContractError"
    finally:
        if driver:
            try:
                driver.close()
            except SmokeFailure:
                report["cleanupWarning"] = "webdriverSessionCloseFailed"
        if arguments.report:
            try:
                arguments.report.parent.mkdir(parents=True, exist_ok=True)
                arguments.report.write_text(json.dumps(report, indent=2, sort_keys=True) + "\n", encoding="utf-8")
            except OSError:
                report["status"] = "failed"
                report["errorCode"] = "smokeReportWriteFailed"
                exit_code = 1
        print(json.dumps(report, sort_keys=True))
    return exit_code


if __name__ == "__main__":
    raise SystemExit(main())
