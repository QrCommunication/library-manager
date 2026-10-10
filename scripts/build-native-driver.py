#!/usr/bin/env python3
"""Build the official QA driver with the repository's reproducible dependency lock."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path, PurePosixPath
import shutil
import subprocess
import tarfile
import urllib.error
import urllib.request


DRIVER_VERSION = "2.1.0"
HYPER_VERSION = "1.12.0"
CRATE_SHA256 = "2f2e29bd6900fb718dfdc06f2e7a371d6087c085daaf83709847ef3e6b0c1953"
CRATE_URL = "https://static.crates.io/crates/tauri-driver/tauri-driver-2.1.0.crate"
LOCK = Path(__file__).with_name("native-driver.lock")
MAX_ARCHIVE_BYTES = 8 * 1024 * 1024
MAX_SOURCE_BYTES = 32 * 1024 * 1024
MAX_SOURCE_ENTRIES = 512
MAX_METADATA_BYTES = 16 * 1024 * 1024
REGISTRY = "registry+https://github.com/rust-lang/crates.io-index"


class BuildFailure(RuntimeError):
    """Static diagnostics only; compiler logs remain in the private work directory."""


def require(condition: bool, code: str) -> None:
    if not condition:
        raise BuildFailure(code)


def file_sha(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for block in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


def fresh_path(value: Path) -> Path:
    require(value.is_absolute() and ".." not in value.parts, "buildPathMustBeAbsolute")
    require(not value.exists() and not value.is_symlink(), "buildDestinationAlreadyExists")
    require(value.parent.is_dir(), "buildDestinationParentMissing")
    return value.parent.resolve(strict=True) / value.name


def private_file(path: Path):
    return os.fdopen(os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600), "wb")


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, *arguments):
        return None


def download_crate(destination: Path) -> None:
    opener = urllib.request.build_opener(urllib.request.ProxyHandler({}), NoRedirect())
    request = urllib.request.Request(CRATE_URL, headers={"User-Agent": "library-manager-qa-driver-build"})
    try:
        with opener.open(request, timeout=120) as response, private_file(destination) as output:
            require(response.status == 200, "officialDriverDownloadStatusInvalid")
            total = 0
            while block := response.read(128 * 1024):
                total += len(block)
                require(total <= MAX_ARCHIVE_BYTES, "officialDriverArchiveTooLarge")
                output.write(block)
    except (urllib.error.HTTPError, urllib.error.URLError):
        raise BuildFailure("officialDriverDownloadFailed") from None
    require(file_sha(destination) == CRATE_SHA256, "officialDriverArchiveChecksumMismatch")


def extract_source(archive_path: Path, destination: Path) -> dict[str, str]:
    """Extract only bounded regular files/directories below the verified crate root."""
    destination.mkdir(mode=0o700, exist_ok=False)
    prefix = "tauri-driver-" + DRIVER_VERSION
    fingerprints = {}
    seen = set()
    total = 0
    with tarfile.open(archive_path, "r:gz") as archive:
        for index, entry in enumerate(archive):
            require(index < MAX_SOURCE_ENTRIES, "officialDriverSourceEntryLimit")
            path = PurePosixPath(entry.name)
            require(not path.is_absolute() and ".." not in path.parts and "\\" not in entry.name
                    and path.parts and path.parts[0] == prefix, "officialDriverSourcePathInvalid")
            relative = Path(*path.parts[1:])
            require(entry.isdir() or entry.isfile(), "officialDriverSourceEntryTypeInvalid")
            require(entry.name not in seen, "officialDriverSourceDuplicateEntry")
            seen.add(entry.name)
            target = destination / relative
            if entry.isdir():
                target.mkdir(mode=0o700, parents=True, exist_ok=True)
                continue
            require(relative.parts, "officialDriverSourceRootNotDirectory")
            total += entry.size
            require(0 <= entry.size <= MAX_SOURCE_BYTES and total <= MAX_SOURCE_BYTES,
                    "officialDriverSourceSizeLimit")
            target.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
            with archive.extractfile(entry) as source, private_file(target) as output:
                written = 0
                while block := source.read(128 * 1024):
                    written += len(block)
                    require(written <= entry.size, "officialDriverSourceEntryTooLarge")
                    output.write(block)
            require(written == entry.size, "officialDriverSourceTruncated")
            fingerprints[relative.as_posix()] = file_sha(target)
    require("Cargo.toml" in fingerprints and "Cargo.lock" in fingerprints and "src/main.rs" in fingerprints,
            "officialDriverSourceIncomplete")
    return fingerprints


def run(command: list[str], work: Path, label: str, timeout: int, environment: dict[str, str]) -> Path:
    output_path = work / (label + ".stdout")
    try:
        with private_file(output_path) as output, private_file(work / (label + ".stderr")) as errors:
            result = subprocess.run(command, stdout=output, stderr=errors, timeout=timeout, env=environment)
    except subprocess.TimeoutExpired:
        raise BuildFailure(label + "Timeout") from None
    require(result.returncode == 0, label + "Failed")
    return output_path


def validate_metadata(metadata: dict, source: Path) -> None:
    packages = metadata.get("packages", [])
    local = [package for package in packages if package.get("source") is None]
    require(len(local) == 1 and local[0]["name"] == "tauri-driver" and local[0]["version"] == DRIVER_VERSION
            and Path(local[0]["manifest_path"]).resolve() == source / "Cargo.toml", "driverMetadataRootInvalid")
    require(all(package.get("source") in (None, REGISTRY) for package in packages), "driverNonRegistryDependency")
    require(metadata.get("workspace_members") == [local[0]["id"]], "driverMetadataWorkspaceInvalid")
    hyper = [package for package in packages if package["name"] == "hyper"]
    require(len(hyper) == 1 and hyper[0]["version"] == HYPER_VERSION and hyper[0]["source"] == REGISTRY,
            "driverHyperVersionInvalid")


def build(arguments) -> dict:
    root, work, report = (fresh_path(value) for value in (arguments.root, arguments.work_dir, arguments.report))
    require(len({root, work, report}) == 3 and root not in work.parents and work not in root.parents
            and root not in report.parents and work not in report.parents, "buildDestinationsMustBeSeparate")
    require(LOCK.is_file() and not LOCK.is_symlink() and LOCK.stat().st_size <= MAX_SOURCE_BYTES,
            "driverRepositoryLockMissing")
    lock_bytes = LOCK.read_bytes()
    lock_sha = hashlib.sha256(lock_bytes).hexdigest()
    cargo = shutil.which(arguments.cargo)
    require(cargo is not None, "driverCargoExecutableMissing")
    environment = dict(os.environ)
    # A custom Cargo path must select its matching rustc/rustup toolchain too.
    environment["PATH"] = str(Path(cargo).parent) + os.pathsep + environment.get("PATH", "")
    environment["CARGO_TARGET_DIR"] = str(work / "target")
    root.mkdir(mode=0o700, exist_ok=False)
    work.mkdir(mode=0o700, exist_ok=False)
    archive = work / "tauri-driver.crate"
    download_crate(archive)
    source = work / "source"
    original = extract_source(archive, source)
    # This is the sole overlay. Official manifests and Rust sources are preserved.
    (source / "Cargo.lock").write_bytes(lock_bytes)
    metadata_path = run([cargo, "metadata", "--locked", "--format-version", "1", "--manifest-path",
                         str(source / "Cargo.toml")], work, "driverMetadata", 180, environment)
    require(metadata_path.stat().st_size <= MAX_METADATA_BYTES, "driverMetadataTooLarge")
    validate_metadata(json.loads(metadata_path.read_text(encoding="utf-8")), source)
    run([cargo, "install", "--path", str(source), "--locked", "--root", str(root)], work,
        "driverInstall", 600, environment)
    require(file_sha(source / "Cargo.lock") == lock_sha and LOCK.read_bytes() == lock_bytes,
            "driverLockChangedDuringBuild")
    require(all(file_sha(source / name) == expected for name, expected in original.items() if name != "Cargo.lock"),
            "officialDriverSourceChangedDuringBuild")
    executable = root / "bin" / ("tauri-driver.exe" if os.name == "nt" else "tauri-driver")
    require(executable.is_file() and not executable.is_symlink() and os.access(executable, os.X_OK),
            "driverBuiltExecutableMissing")
    rustc = shutil.which("rustc", path=environment["PATH"])
    require(rustc is not None, "driverRustcExecutableMissing")
    version_path = run([rustc, "--version"], work, "driverRustcVersion", 15, environment)
    rustc_version = version_path.read_text(encoding="utf-8").strip()
    require(rustc_version.startswith("rustc ") and len(rustc_version) < 200, "driverRustcVersionInvalid")
    proof = {"schemaVersion": 1, "status": "passed", "driverVersion": DRIVER_VERSION,
             "sourceArchiveSha256": CRATE_SHA256, "lockSha256": lock_sha,
             "hyperVersion": HYPER_VERSION, "driverSha256": file_sha(executable),
             "rustcVersion": rustc_version, "officialSourceUnmodified": True, "lockedInstall": True,
             "registryDependenciesOnly": True, "requestRetriesAdded": False}
    with private_file(report) as output:
        output.write((json.dumps(proof, indent=2, sort_keys=True) + "\n").encode())
    return proof


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, required=True)
    parser.add_argument("--work-dir", type=Path, required=True)
    parser.add_argument("--report", type=Path, required=True)
    parser.add_argument("--cargo", default="cargo")
    arguments = parser.parse_args()
    try:
        proof = build(arguments)
    except (BuildFailure, OSError, ValueError, KeyError, TypeError, tarfile.TarError) as error:
        print(json.dumps({"status": "failed", "errorCode": str(error) if type(error) is BuildFailure
                          else "driverBuildInfrastructureError"}))
        return 1
    print(json.dumps(proof, sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
