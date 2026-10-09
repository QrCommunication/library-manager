#!/usr/bin/env python3
"""Validate USB inventory/import against a real mounted reader without writing it.

Start tauri-driver with XDG_DATA_HOME pointing at the fresh --profile-root before
calling this script. The application's bootstrap must confirm that isolation.
Reports never contain device paths, book titles, device IDs or book hashes.
Optional screenshots contain private library contents and are written mode 0600.
"""
from __future__ import annotations

import argparse
import base64
from collections import Counter
import importlib.util
import json
import os
from pathlib import Path
import tempfile
import time
import unittest

_SPEC = importlib.util.spec_from_file_location("library_manager_native_smoke", Path(__file__).with_name("native-smoke.py"))
assert _SPEC is not None and _SPEC.loader is not None
_NATIVE = importlib.util.module_from_spec(_SPEC)
_SPEC.loader.exec_module(_NATIVE)
Driver = _NATIVE.Driver
SmokeFailure = _NATIVE.SmokeFailure
IpcFailure = _NATIVE.IpcFailure
require = _NATIVE.require
isolated_profile = _NATIVE.isolated_profile
file_sha256 = _NATIVE.file_sha256
checked = _NATIVE.checked
QUERY = _NATIVE.QUERY
TERMINAL = _NATIVE.TERMINAL_JOBS
PAGE_SIZE = 200
MAX_BOOKS = 20_000
MAX_FILE_BYTES = 512 * 1024 * 1024
MAX_SAFE_INTEGER = (1 << 53) - 1

UNKNOWN_DOM = """
const section = document.querySelector('#main-content .device-library');
section?.scrollIntoView({block:'start',behavior:'instant'});
const visible = element => {
  const rect = element?.getBoundingClientRect();
  if (!rect) return false;
  const style = getComputedStyle(element);
  return rect.width > 0 && rect.height > 0 && rect.bottom > 0 && rect.top < innerHeight &&
    rect.right > 0 && rect.left < innerWidth && style.display !== 'none' &&
    style.visibility !== 'hidden' && Number(style.opacity) > 0;
};
const rows = section ? [...section.querySelectorAll('.device-books > li')] : [];
const badges = rows.filter(row => visible(row) && row.querySelector('.device-badge') &&
  row.querySelector('button:not(:disabled)')).length;
return {visible:visible(section),renderedRows:rows.length,visibleImportableRows:badges,
  localRows:document.querySelectorAll('#main-content .book-grid .book-card').length,
  demo:new URL(location.href).searchParams.has('demo')};
"""


def integer(value, code: str) -> int:
    require(type(value) is int and 0 <= value <= MAX_SAFE_INTEGER, code)
    return value


def parse_progress(value) -> dict:
    require(isinstance(value, dict), "indexProgressShapeInvalid")
    phase = value.get("phase")
    require(isinstance(phase, str) and phase in {"discovering", "reading", "finalizing"}, "indexProgressPhaseInvalid")
    result = {"phase": phase}
    for key in ("visitedEntries", "processedBooks", "totalBooks", "bytesRead", "totalBytes"):
        result[key] = integer(value.get(key), "indexProgressCounterInvalid")
    require(result["processedBooks"] <= result["totalBooks"] and result["bytesRead"] <= result["totalBytes"], "indexProgressCounterOverflow")
    require(value.get("currentPath") is None or isinstance(value.get("currentPath"), str), "indexProgressPathInvalid")
    # Deliberately discard currentPath; all returned fields are safe to publish.
    return result


class ProgressSamples:
    def __init__(self) -> None:
        self.samples: list[dict] = []
        self.fixed_total: int | None = None

    def add(self, raw) -> None:
        sample = parse_progress(raw)
        if self.samples:
            previous = self.samples[-1]
            phases = {"discovering": 0, "reading": 1, "finalizing": 2}
            require(phases[sample["phase"]] >= phases[previous["phase"]], "indexProgressPhaseWentBackwards")
            for key in ("visitedEntries", "processedBooks", "totalBooks", "bytesRead"):
                require(sample[key] >= previous[key], "indexProgressWentBackwards")
        if sample["phase"] != "discovering":
            if self.fixed_total is None:
                self.fixed_total = sample["totalBytes"]
            require(sample["totalBytes"] == self.fixed_total, "indexReadingDenominatorChanged")
        if not self.samples or sample != self.samples[-1]:
            require(len(self.samples) < 10_000, "indexProgressSampleLimitExceeded")
            self.samples.append(sample)


def validate_page(page, offset: int, expected_total: int | None = None) -> tuple[list[dict], int]:
    require(isinstance(page, dict) and isinstance(page.get("items"), list), "inventoryPageShapeInvalid")
    total = integer(page.get("total"), "inventoryTotalInvalid")
    require(total <= MAX_BOOKS and integer(page.get("offset"), "inventoryOffsetInvalid") == offset, "inventoryPaginationInvalid")
    require(integer(page.get("limit"), "inventoryLimitInvalid") == PAGE_SIZE, "inventoryPageLimitInvalid")
    require(len(page["items"]) <= PAGE_SIZE and offset + len(page["items"]) <= total, "inventoryPageCountInvalid")
    if expected_total is not None:
        require(total == expected_total, "completeInventoryTotalChanged")
    for item in page["items"]:
        require(isinstance(item, dict), "inventoryBookShapeInvalid")
        relative = item.get("relativePath")
        require(isinstance(relative, str) and relative and not Path(relative).is_absolute()
                and ".." not in Path(relative).parts and not any(ord(char) < 32 for char in relative), "inventoryBookPathInvalid")
        require(isinstance(item.get("format"), str), "inventoryBookFormatInvalid")
        integer(item.get("sizeBytes"), "inventoryBookSizeInvalid")
        require(item.get("bookId") is None or isinstance(item.get("bookId"), str), "inventoryBookLocalIdInvalid")
    return page["items"], total


def inventory(driver: Driver, device_id: str, unknown_only: bool) -> tuple[list[dict], int]:
    rows: list[dict] = []
    total = None
    while total is None or len(rows) < total:
        page = driver.invoke("device_inventory", {"id": device_id, "offset": len(rows), "limit": PAGE_SIZE, "unknownOnly": unknown_only})
        values, total = validate_page(page, len(rows), total)
        require(values or len(rows) == total, "inventoryPageDidNotAdvance")
        rows.extend(values)
    require(len(rows) == total and len({row["relativePath"] for row in rows}) == total, "inventoryPaginationDuplicatedOrLostBooks")
    return rows, total


def select_device(devices, requested: str | None) -> dict:
    require(isinstance(devices, list), "devicesShapeInvalid")
    available = [device for device in devices if isinstance(device, dict) and device.get("transport") == "usb" and device.get("connected") is True]
    if requested:
        available = [device for device in available if device.get("id") == requested]
    require(len(available) == 1, "connectedUsbDeviceMustBeUniqueOrExplicit")
    require(isinstance(available[0].get("id"), str) and isinstance(available[0].get("mountPath"), str), "usbDeviceCapabilityMissing")
    return available[0]


def resolve_source(root: Path, relative: str) -> Path:
    path = root / relative
    require(not Path(relative).is_absolute() and ".." not in Path(relative).parts, "unsafeImportCandidatePath")
    require(root.is_dir() and not root.is_symlink(), "usbMountInvalid")
    current = root
    for part in Path(relative).parts:
        current = current / part
        require(not current.is_symlink(), "usbSourceSymlinkRefused")
    require(path.is_file() and path.resolve().is_relative_to(root.resolve()), "usbSourceEscapesMount")
    return path


def output_path(path: Path, device_root: Path | None = None) -> Path:
    require(path.is_absolute() and not path.is_symlink(), "outputMustBeAbsoluteAndNotSymlink")
    resolved = path.resolve()
    require(resolved.is_relative_to(Path(tempfile.gettempdir()).resolve()) and resolved != Path(tempfile.gettempdir()).resolve(), "smokeOutputsMustBeTemporary")
    if device_root is not None:
        require(not resolved.is_relative_to(device_root.resolve()), "smokeOutputWouldWriteDevice")
    return resolved


def private_write(path: Path, data: bytes) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    descriptor = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_TRUNC | os.O_NOFOLLOW, 0o600)
    try:
        os.fchmod(descriptor, 0o600)
        with os.fdopen(descriptor, "wb", closefd=False) as stream:
            stream.write(data)
    finally:
        os.close(descriptor)


def capture_screenshot(driver: Driver, target: Path) -> None:
    encoded = driver.request("GET", driver.endpoint("/screenshot"))
    require(isinstance(encoded, str), "nativeScreenshotResponseInvalid")
    try:
        data = base64.b64decode(encoded, validate=True)
    except ValueError:
        raise SmokeFailure("nativeScreenshotBase64Invalid") from None
    require(data.startswith(b"\x89PNG\r\n\x1a\n"), "nativeScreenshotNotPng")
    private_write(target, data)


def smoke(driver: Driver, binary: Path, profile: Path, requested: str | None, screenshot: Path | None, report: dict, roots: list[Path]) -> None:
    with checked(report, "nativeBootstrapUsesFreshTemporaryProfile"):
        driver.start(binary)
        bootstrap = driver.invoke("app_bootstrap")
        require(isinstance(bootstrap, dict) and isinstance(bootstrap.get("version"), str), "bootstrapVersionMissing")
        settings = driver.invoke("settings_get")
        require(settings == bootstrap.get("settings"), "bootstrapSettingsDiverged")
        library_root = Path(settings["libraryRoot"]).resolve()
        require(library_root.is_relative_to(profile) and library_root != profile, "applicationDidNotUseIsolatedProfile")
        require(driver.invoke("library_list", {"query": QUERY})["total"] == 0, "isolatedLibraryWasNotEmpty")
        settings.update({"language": "fr", "autoEnrich": False, "webEnabled": False, "providerId": None, "modelId": None, "maxConcurrentJobs": 1})
        driver.invoke("settings_save", {"settings": settings})
        report["version"] = bootstrap["version"]

    with checked(report, "automaticUsbInventoryHasMeasuredProgressAndPendingBooks"):
        device = select_device(driver.invoke("devices_scan"), requested)
        device_id = device["id"]
        root = Path(device["mountPath"])
        roots.append(root)
        require(not profile.is_relative_to(root.resolve()), "isolatedProfileWouldWriteDevice")
        if screenshot:
            output_path(screenshot, root)
        samples = ProgressSamples()
        index_id = None
        pending_books = 0
        pending_observed = False
        completed = None

        def observe():
            nonlocal index_id, pending_books, pending_observed, completed
            jobs = driver.invoke("jobs_list")
            require(isinstance(jobs, list), "jobsShapeInvalid")
            candidates = [job for job in jobs if isinstance(job, dict) and job.get("kind") == "deviceIndex"
                          and isinstance(job.get("result"), dict) and job["result"].get("deviceId") == device_id]
            if index_id is None and candidates:
                index_id = candidates[-1]["id"]
            matches = [job for job in candidates if job.get("id") == index_id]
            if not matches:
                return False
            job = matches[0]
            result = job["result"]
            if "indexProgress" in result:
                samples.add(result["indexProgress"])
            if job.get("status") in TERMINAL:
                require(job["status"] == "completed", "automaticDeviceIndexFailed")
                completed = job
                return True
            page = driver.invoke("device_inventory", {"id": device_id, "offset": 0, "limit": PAGE_SIZE, "unknownOnly": False})
            rows, total = validate_page(page, 0)
            if rows:
                pending_observed = True
                pending_books = max(pending_books, total)
            return False

        driver.wait(observe, "automaticUsbIndexTimedOut", driver.remaining())
        require(samples.samples, "automaticIndexProgressNotObserved")
        require(any(sample["phase"] == "reading" for sample in samples.samples), "readingProgressNotObserved")
        require(pending_observed, "pendingInventoryNotObserved")
        report["progressSamples"] = samples.samples
        report["pendingInventoryBookCount"] = pending_books
        report["automaticIndexCompleted"] = completed is not None
        report["progressBytesAdvanced"] = len({sample["bytesRead"] for sample in samples.samples}) > 1
        require(report["progressBytesAdvanced"], "measuredReadingBytesDidNotAdvance")

    with checked(report, "completeInventoryPaginationAndDeviceOnlyBooksVisibleWithEmptyLibrary"):
        all_rows, total = inventory(driver, device_id, False)
        unknown_rows, unknown_total = inventory(driver, device_id, True)
        require(total > 0 and unknown_total == total, "freshProfileInventoryWasNotAllUnknown")
        require(driver.invoke("library_list", {"query": QUERY})["total"] == 0, "inventoryCreatedLocalBooks")
        rendered = driver.wait(lambda: (lambda value: value if isinstance(value, dict) and value.get("visible") is True
                            and value.get("visibleImportableRows", 0) > 0 and value.get("localRows") == 0 and not value.get("demo") else False)
                            (driver.execute(UNKNOWN_DOM)), "deviceOnlyBooksNotVisibleWithEmptyLocalLibrary", 40)
        report["inventoryBookCount"] = total
        report["unknownBeforeImport"] = unknown_total
        report["inventoryPageCount"] = (total + PAGE_SIZE - 1) // PAGE_SIZE
        report["renderedDeviceOnlyRows"] = rendered["renderedRows"]
        report["visibleDeviceOnlyImportableRows"] = rendered["visibleImportableRows"]
        if screenshot:
            capture_screenshot(driver, screenshot)
            report["privateScreenshotSaved"] = True

    with checked(report, "oneSmallSupportedBookImportedWithoutWritingSource"):
        occurrences = Counter(row.get("sha256") for row in all_rows)
        choices = [row for row in unknown_rows if row["format"] in {"epub", "txt"}
                   and 0 < row["sizeBytes"] <= MAX_FILE_BYTES and isinstance(row.get("sha256"), str)
                   and occurrences[row["sha256"]] == 1]
        require(choices, "noUniqueSmallSupportedImportCandidate")
        candidate = min(choices, key=lambda row: row["sizeBytes"])
        source = resolve_source(root, candidate["relativePath"])
        before = file_sha256(source)
        require(before == candidate["sha256"], "usbCandidateChangedSinceInventory")
        result = driver.job(driver.invoke("device_import", {"id": device_id, "relativePaths": [candidate["relativePath"]]})).get("result")
        require(isinstance(result, dict) and result.get("imported") == 1 and result.get("duplicates") == 0
                and not result.get("errorsByCode"), "singleDeviceImportFailed")
        page = driver.invoke("library_list", {"query": QUERY})
        require(page["total"] == 1 and len(page["items"]) == 1, "singleDeviceImportLocalCountInvalid")
        require(device_id in page["items"][0].get("onDeviceIds", []), "importedBookDevicePresenceMissing")
        _, after_unknown = inventory(driver, device_id, True)
        require(after_unknown == unknown_total - 1, "unknownCountDidNotDecreaseByOne")
        require(file_sha256(source) == before, "deviceSourceChangedAfterImport")
        report.update({"localBookCount": 1, "unknownAfterImport": after_unknown, "importedBookOnDevice": True,
                       "selectedFormat": candidate["format"], "selectedSizeBytes": candidate["sizeBytes"], "sourceUnchangedAfterImport": True})

    with checked(report, "repeatedDeviceImportReturnsDuplicateAndPreservesSource"):
        repeated = driver.job(driver.invoke("device_import", {"id": device_id, "relativePaths": [candidate["relativePath"]]})).get("result")
        require(isinstance(repeated, dict) and repeated.get("duplicates") == 1 and repeated.get("imported") == 0
                and not repeated.get("errorsByCode"), "repeatedDeviceImportNotDeduplicated")
        require(driver.invoke("library_list", {"query": QUERY})["total"] == 1, "repeatedImportCreatedAnotherBook")
        _, final_unknown = inventory(driver, device_id, True)
        require(final_unknown == after_unknown, "repeatedImportChangedUnknownCount")
        require(file_sha256(source) == before, "deviceSourceChangedAfterRepeatedImport")
        report.update({"repeatedImportDuplicates": 1, "sourceUnchangedAfterRepeatedImport": True, "finalUnknownBookCount": final_unknown})


class SmokeTests(unittest.TestCase):
    def test_progress_strips_private_path_and_checks_counters(self):
        sample = {"phase": "reading", "visitedEntries": 9, "processedBooks": 1, "totalBooks": 3,
                  "bytesRead": 256, "totalBytes": 900, "currentPath": "PRIVATE/TITLE.epub"}
        self.assertNotIn("PRIVATE", json.dumps(parse_progress(sample)))
        for key, value in [("bytesRead", -1), ("totalBytes", True), ("processedBooks", 4), ("phase", "invented")]:
            with self.subTest(key=key):
                invalid = sample | {key: value}
                with self.assertRaises(SmokeFailure):
                    parse_progress(invalid)

    def test_progress_monotonic_and_fixed_read_total(self):
        samples = ProgressSamples()
        first = {"phase": "reading", "visitedEntries": 9, "processedBooks": 0, "totalBooks": 3,
                 "bytesRead": 256, "totalBytes": 900, "currentPath": None}
        samples.add(first)
        samples.add(first | {"bytesRead": 512})
        for invalid in (first, first | {"bytesRead": 600, "totalBytes": 1000}, first | {"phase": "discovering", "bytesRead": 600}):
            with self.assertRaises(SmokeFailure):
                samples.add(invalid)

    def test_pagination_and_unique_device_selection(self):
        row = {"relativePath": "synthetic.txt", "format": "txt", "sizeBytes": 1, "bookId": None}
        self.assertEqual(validate_page({"items": [row], "total": 1, "offset": 0, "limit": 200}, 0), ([row], 1))
        for page in ({"items": [], "total": -1, "offset": 0, "limit": 200},
                     {"items": [row | {"relativePath": "../private"}], "total": 1, "offset": 0, "limit": 200}):
            with self.assertRaises(SmokeFailure):
                validate_page(page, 0)
        device = {"id": "usb-synthetic", "transport": "usb", "connected": True, "mountPath": "/synthetic"}
        self.assertEqual(select_device([device], None), device)
        with self.assertRaises(SmokeFailure):
            select_device([device, device | {"id": "usb-other"}], None)
        self.assertEqual(select_device([device, device | {"id": "usb-other"}], "usb-other")["id"], "usb-other")

    def test_source_resolution_refuses_symlink_and_outputs_refuse_device(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary) / "card"
            root.mkdir()
            book = root / "synthetic.txt"
            book.write_text("synthetic", encoding="utf-8")
            self.assertEqual(resolve_source(root, "synthetic.txt"), book)
            link = root / "link.txt"
            link.symlink_to(book)
            with self.assertRaises(SmokeFailure):
                resolve_source(root, "link.txt")
            with self.assertRaises(SmokeFailure):
                output_path(root / "report.json", root)
            output = Path(temporary) / "private.png"
            private_write(output, b"synthetic")
            self.assertEqual(output.stat().st_mode & 0o777, 0o600)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--driver-url", default="http://127.0.0.1:4444")
    parser.add_argument("--binary", type=Path)
    parser.add_argument("--profile-root", default=os.environ.get("XDG_DATA_HOME"))
    parser.add_argument("--report", type=Path)
    parser.add_argument("--screenshot", type=Path, help="Optional PRIVATE screenshot written mode 0600 under /tmp")
    parser.add_argument("--device-id")
    parser.add_argument("--timeout", type=float, default=600)
    parser.add_argument("--self-test", action="store_true")
    args = parser.parse_args()
    if args.self_test:
        result = unittest.TextTestRunner(verbosity=2).run(unittest.defaultTestLoader.loadTestsFromTestCase(SmokeTests))
        return 0 if result.wasSuccessful() else 1
    if not args.binary or not args.profile_root or not args.report:
        parser.error("--binary, fresh --profile-root and --report are required")
    report = {"schemaVersion": 1, "status": "running", "checks": [], "paidApiCalls": 0,
              "physicalDeviceWrites": 0, "privateScreenshotSaved": False, "hardwareSmoke": True}
    driver = None
    roots: list[Path] = []
    report_target = None
    exit_code = 1
    try:
        require(60 <= args.timeout <= 3600, "smokeTimeoutOutOfRange")
        require(isinstance(args.profile_root, str) and Path(args.profile_root).resolve().is_relative_to(Path(tempfile.gettempdir()).resolve()), "profileMustBeTemporary")
        binary = args.binary.resolve(strict=True)
        require(binary.is_file() and os.access(binary, os.X_OK), "nativeApplicationNotExecutable")
        report_target = output_path(args.report)
        screenshot = output_path(args.screenshot) if args.screenshot else None
        profile = isolated_profile(args.profile_root)
        driver = Driver(args.driver_url, args.timeout)
        smoke(driver, binary, profile, args.device_id, screenshot, report, roots)
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
        if report_target:
            try:
                for root in roots:
                    output_path(report_target, root)
                private_write(report_target, (json.dumps(report, indent=2, sort_keys=True) + "\n").encode())
            except (OSError, SmokeFailure):
                report["status"] = "failed"
                report["errorCode"] = "smokeReportWriteFailed"
                exit_code = 1
        print(json.dumps(report, sort_keys=True))
    return exit_code


if __name__ == "__main__":
    raise SystemExit(main())
