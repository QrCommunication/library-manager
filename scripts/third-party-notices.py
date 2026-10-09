#!/usr/bin/env python3
"""Build-only, deterministic notices from locked Cargo and installed JS sources."""

from __future__ import annotations

import argparse
from dataclasses import dataclass, field
import hashlib
import json
from pathlib import Path
import re
import subprocess
import sys
import tempfile
import unittest
from urllib.parse import urlsplit, urlunsplit


ROOT = Path(__file__).resolve().parents[1]
MAX_LICENSE_BYTES = 1024 * 1024
REQUIRED_JS = {"svelte", "@lucide/svelte"}
REQUIRED_CARGO_NOTICES = {"dlopen2", "alloc-stdlib"}
LICENSE_NAME = re.compile(r"^(?:licen[cs]e|copying|notice|copyright)(?:[._-].*)?$", re.I)


@dataclass
class Dependency:
    ecosystem: str
    name: str
    version: str
    license: str
    repository: str
    texts: dict[str, str] = field(default_factory=dict)
    warnings: list[str] = field(default_factory=list)
    provenance: list[dict[str, str]] = field(default_factory=list)


def normalize_text(text: str) -> str:
    return "\n".join(line.rstrip() for line in text.replace("\r\n", "\n").replace("\r", "\n").splitlines()).strip() + "\n"


def safe_repository(value: object) -> str:
    if isinstance(value, dict):
        value = value.get("url", "")
    if not isinstance(value, str):
        return ""
    value = value.removeprefix("git+")
    if value.startswith("git@github.com:"):
        value = "https://github.com/" + value.split(":", 1)[1]
    try:
        url = urlsplit(value)
        if url.scheme not in {"https", "http"} or not url.hostname or url.username or url.password or any(ord(char) < 32 for char in value):
            return ""
        return urlunsplit((url.scheme, url.netloc, url.path, "", ""))
    except ValueError:
        return ""


def read_license(path: Path, package_root: Path) -> str | None:
    try:
        resolved = path.resolve(strict=True)
        resolved.relative_to(package_root.resolve())
        if not resolved.is_file() or resolved.stat().st_size > MAX_LICENSE_BYTES:
            return None
        text = resolved.read_text(encoding="utf-8")
        if "\0" in text or not text.strip():
            return None
        return normalize_text(text)
    except (OSError, UnicodeError, ValueError):
        return None


def license_texts(package_root: Path, declared_file: str | None = None) -> dict[str, str]:
    candidates: set[Path] = set()
    if declared_file:
        candidates.add(package_root / declared_file)
    try:
        for item in package_root.iterdir():
            if item.is_file() and LICENSE_NAME.fullmatch(item.name):
                candidates.add(item)
            elif item.name.lower() in {"license", "licenses", "licence", "licences"} and item.is_dir():
                for nested in item.iterdir():
                    if nested.is_file():
                        candidates.add(nested)
                    elif nested.is_dir():
                        candidates.update(child for child in nested.iterdir() if child.is_file())
    except OSError:
        return {}
    texts: dict[str, str] = {}
    for path in sorted(candidates):
        text = read_license(path, package_root)
        if text is not None:
            try:
                label = path.relative_to(package_root).as_posix()
            except ValueError:
                label = path.name
            texts[label] = text
    return texts


def frozen_notices(root: Path, package: dict, package_root: Path) -> tuple[dict[str, str], list[dict[str, str]]]:
    """Verify committed originals against the exact cached crate's VCS revision."""
    directory = root / "vendor" / "licenses"
    manifest = directory / "manifest.json"
    if not manifest.is_file():
        return {}, []
    try:
        if manifest.stat().st_size > 64 * 1024:
            raise ValueError("manifest size")
        document = json.loads(manifest.read_text(encoding="utf-8"))
        entries = document["entries"]
        if document["schemaVersion"] != 1 or not isinstance(entries, list) or len(entries) > 64:
            raise ValueError("manifest schema")
        matches = [entry for entry in entries if isinstance(entry, dict) and entry.get("name") == package["name"] and entry.get("version") == package["version"]]
        if not matches:
            return {}, []
        if len(matches) != 1:
            raise ValueError("duplicate entry")
        entry = matches[0]
        required = ("name", "version", "license", "repository", "revision", "pathInVcs", "source", "file", "sha256")
        if any(not isinstance(entry.get(key), str) for key in required):
            raise ValueError("entry types")
        repository = safe_repository(package.get("repository"))
        revision = entry["revision"]
        if not repository.startswith("https://github.com/") or entry["repository"] != repository or entry["license"] != package.get("license") or not re.fullmatch(r"[0-9a-f]{40}", revision):
            raise ValueError("package provenance")
        source = repository.replace("https://github.com/", "https://raw.githubusercontent.com/", 1) + f"/{revision}/LICENSE"
        if entry["source"] != source or not re.fullmatch(r"[0-9a-f]{64}", entry["sha256"]):
            raise ValueError("source provenance")
        vcs = json.loads((package_root / ".cargo_vcs_info.json").read_text(encoding="utf-8"))
        if vcs["git"]["sha1"] != revision or vcs.get("path_in_vcs", "") != entry["pathInVcs"]:
            raise ValueError("crate revision mismatch")
        filename = Path(entry["file"])
        if filename.is_absolute() or len(filename.parts) != 1 or filename.name in {".", ".."}:
            raise ValueError("license path")
        path = directory / filename
        text = read_license(path, directory)
        if text is None or hashlib.sha256(path.read_bytes()).hexdigest() != entry["sha256"]:
            raise ValueError("license integrity")
        return {f"Upstream LICENSE ({revision[:12]})": text}, [entry]
    except (OSError, UnicodeError, ValueError, KeyError, TypeError) as error:
        raise RuntimeError(f"Frozen license provenance or integrity is invalid for {package['name']} {package['version']}") from error


def cargo_dependencies(root: Path) -> list[Dependency]:
    command = ["cargo", "metadata", "--locked", "--offline", "--format-version", "1"]
    try:
        process = subprocess.run(command, cwd=root, capture_output=True, text=True, check=False, timeout=180)
    except (OSError, subprocess.TimeoutExpired) as error:
        raise RuntimeError("Cargo metadata is unavailable; install the pinned build toolchain and dependency cache") from error
    if process.returncode:
        raise RuntimeError("cargo metadata --locked failed; run that command locally to inspect build diagnostics")
    try:
        metadata = json.loads(process.stdout)
    except json.JSONDecodeError as error:
        raise RuntimeError("Cargo returned malformed metadata") from error
    workspace = set(metadata["workspace_members"])
    dependencies = []
    for package in metadata["packages"]:
        if package["id"] in workspace:
            continue
        package_root = Path(package["manifest_path"]).parent
        texts = license_texts(package_root, package.get("license_file"))
        provenance = []
        if not texts:
            texts, provenance = frozen_notices(root, package, package_root)
        dependency = Dependency("Cargo", package["name"], package["version"], package.get("license") or "Not declared", safe_repository(package.get("repository")), texts)
        dependency.provenance = provenance
        if package["name"] in REQUIRED_CARGO_NOTICES and not texts:
            raise RuntimeError(f"Required Linux runtime package {package['name']} {package['version']} is missing its original license notice")
        if not texts:
            dependency.warnings.append("The cached source package does not include a readable license file. The SPDX declaration is recorded; no missing text is invented.")
        if not package.get("license"):
            dependency.warnings.append("The package metadata does not declare an SPDX expression; consult its supplied license text.")
        dependencies.append(dependency)
    return dependencies


def module_roots(directory: Path) -> list[Path]:
    if not directory.is_dir():
        return []
    packages = []
    for child in sorted(directory.iterdir()):
        if child.name.startswith("."):
            continue
        if child.name.startswith("@") and child.is_dir():
            packages.extend(nested for nested in sorted(child.iterdir()) if (nested / "package.json").is_file())
        elif (child / "package.json").is_file():
            packages.append(child)
    return packages


def js_dependencies(root: Path) -> list[Dependency]:
    modules = root / "node_modules"
    if not modules.is_dir():
        raise RuntimeError("Install the locked JS dependencies with pnpm before generating notices")
    pending = module_roots(modules)
    store = modules / ".pnpm"
    if store.is_dir():
        for entry in sorted(store.iterdir()):
            pending.extend(module_roots(entry / "node_modules"))
    seen: set[Path] = set()
    packages: dict[tuple[str, str], Dependency] = {}
    while pending:
        package_root = pending.pop().resolve()
        if package_root in seen:
            continue
        seen.add(package_root)
        try:
            package = json.loads((package_root / "package.json").read_text(encoding="utf-8"))
        except (OSError, UnicodeError, json.JSONDecodeError) as error:
            raise RuntimeError("An installed JS package has unreadable metadata") from error
        name, version = package.get("name"), package.get("version")
        if not isinstance(name, str) or not isinstance(version, str):
            raise RuntimeError("An installed JS package lacks its name or version")
        license_value = package.get("license")
        if isinstance(license_value, dict):
            license_value = license_value.get("type")
        texts = license_texts(package_root)
        dependency = Dependency("npm", name, version, license_value if isinstance(license_value, str) else "Not declared", safe_repository(package.get("repository")), texts)
        if not texts:
            dependency.warnings.append("The installed package does not supply a readable license file; its declared license is listed without inventing text.")
        if not license_value:
            dependency.warnings.append("The package metadata does not declare an SPDX expression.")
        key = (name, version)
        if key in packages:
            packages[key].texts.update(texts)
            if packages[key].texts:
                packages[key].warnings = [warning for warning in packages[key].warnings if "readable license file" not in warning]
        else:
            packages[key] = dependency
        pending.extend(module_roots(package_root / "node_modules"))
    for name in sorted(REQUIRED_JS):
        required = [package for package in packages.values() if package.name == name]
        if not required or any(not package.texts or package.license == "Not declared" for package in required):
            raise RuntimeError(f"Required shipped package {name} is missing its license declaration or text")
    return list(packages.values())


def vendor_dependencies(root: Path) -> list[Dependency]:
    dependencies = []
    libraries = sorted((root / "vendor").glob("libmobi-*"))
    if not libraries:
        raise RuntimeError("The bundled libmobi source and COPYING are missing")
    for library in libraries:
        copying = read_license(library / "COPYING", library)
        if not copying:
            raise RuntimeError("Bundled libmobi COPYING is missing or unreadable")
        texts = {"COPYING": copying}
        header = (library / "src" / "mobi.h").read_text(encoding="utf-8")
        copyright_lines = [line.strip(" *") for line in header.splitlines()[:30] if "Copyright" in line]
        if copyright_lines:
            texts["src/mobi.h copyright"] = normalize_text("\n".join(copyright_lines))
        dependencies.append(Dependency("Bundled", "libmobi", library.name.removeprefix("libmobi-"), "LGPL-3.0-or-later", "https://github.com/bfabiszewski/libmobi", texts))
        miniz_path = library / "src" / "miniz.c"
        source = miniz_path.read_text(encoding="utf-8")
        marker = "This is free and unencumbered software released into the public domain."
        start = source.rfind(marker)
        end = source.find("*/", start)
        version = re.search(r"miniz\.c v([\d.]+)", source)
        if start < 0 or end < start or not version:
            raise RuntimeError("The bundled miniz Unlicense notice or version is missing")
        dependencies.append(Dependency("Bundled", "miniz (libmobi copy)", version.group(1), "Unlicense", "https://github.com/richgel999/miniz", {"src/miniz.c Unlicense": normalize_text(source[start:end])}))
    return dependencies


def markdown(value: str) -> str:
    return value.replace("\r", " ").replace("\n", " ").replace("|", "\\|").replace("<", "&lt;").replace(">", "&gt;").replace("[", "\\[").replace("]", "\\]")


def render(dependencies: list[Dependency]) -> str:
    dependencies = sorted(dependencies, key=lambda item: (item.ecosystem, item.name.casefold(), item.version))
    texts = {hashlib.sha256(text.encode()).hexdigest(): text for dependency in dependencies for text in dependency.texts.values()}
    output = [
        "# Third-party notices", "",
        "Generated by `python3 scripts/third-party-notices.py` from `cargo metadata --locked --offline`, installed npm/pnpm package sources, and the bundled libmobi source. Run after installing the locked dependencies; `--check` verifies that this document is current without downloading notices.", "",
        "The inventory includes build tools and dependencies resolved for other platforms. Their presence in this list does not mean that every package is included in each Linux binary. License declarations come from package metadata; original supplied notices and copyright statements are preserved below. Identical texts are shared by hash without replacing package-specific attribution. No build directory, user profile path, secret, or generation timestamp is added.", "",
        "Library Manager itself is licensed under GPL-3.0-only; its complete application license is in `LICENSE`. Bundled libmobi is LGPL-3.0-or-later and its corresponding source remains in `vendor/`.", "",
        "## Dependency inventory", "",
        "| Ecosystem | Package | Version | Declared SPDX/license | Repository | Supplied notices |",
        "| --- | --- | --- | --- | --- | --- |",
    ]
    for dependency in dependencies:
        links = []
        for label, text in sorted(dependency.texts.items()):
            digest = hashlib.sha256(text.encode()).hexdigest()
            links.append(f"[{markdown(label)}](#license-{digest[:16]})")
        repository = f"<{dependency.repository}>" if dependency.repository else "Not declared"
        output.append(f"| {markdown(dependency.ecosystem)} | {markdown(dependency.name)} | {markdown(dependency.version)} | {markdown(dependency.license)} | {repository} | {'; '.join(links) or 'Unavailable in supplied package'} |")
    warnings = [(dependency, warning) for dependency in dependencies for warning in dependency.warnings]
    output.extend(["", "## Source inventory limitations", ""])
    if warnings:
        output.append("The following declarations are recorded accurately, but the installed/cached package did not provide every original notice. These entries may include platform-specific or build-only dependencies; no assertion that they are shipped is made. Inspect the corresponding upstream source before distributing a target that requires a missing notice.")
        output.append("")
        for dependency, warning in warnings:
            output.append(f"- {markdown(dependency.ecosystem)} `{markdown(dependency.name)} {markdown(dependency.version)}`: {warning}")
    else:
        output.append("All inventoried packages supplied readable license text.")
    provenance = [(dependency, entry) for dependency in dependencies for entry in dependency.provenance]
    if provenance:
        output.extend(["", "## Frozen upstream notice sources", "",
            "For these Cargo archives, the upstream repository supplied the notice omitted from the archive. The committed original is verified against the crate's published VCS revision and the raw file SHA-256; regeneration requires no network request.", "",
            "| Package | Version | Revision | Original source | Raw SHA-256 |", "| --- | --- | --- | --- | --- |"])
        for dependency, entry in provenance:
            output.append(f"| {markdown(dependency.name)} | {markdown(dependency.version)} | `{entry['revision']}` | <{entry['source']}> | `{entry['sha256']}` |")
    output.extend(["", "## Original license and notice texts", ""])
    for digest, text in sorted(texts.items()):
        longest = max((len(match.group()) for match in re.finditer(r"`+", text)), default=0)
        fence = "`" * max(3, longest + 1)
        output.extend([f'<a id="license-{digest[:16]}"></a>', "", f"### License text {digest[:16]}", "", f"{fence}text", text.rstrip(), fence, ""])
    return "\n".join(output).rstrip() + "\n"


class GeneratorTests(unittest.TestCase):
    def frozen_fixture(self, directory: Path) -> tuple[Path, dict, Path]:
        root = directory / "project"
        licenses = root / "vendor" / "licenses"
        licenses.mkdir(parents=True)
        package_root = directory / "cached-crate"
        package_root.mkdir()
        revision = "a" * 40
        repository = "https://github.com/example/public-source"
        text = b"Copyright Original Holder\nPermission notice from the original repository.\n"
        (licenses / "fixture-LICENSE").write_bytes(text)
        entry = {"name": "fixture", "version": "1.0", "license": "MIT", "repository": repository, "revision": revision, "pathInVcs": "crate", "source": f"https://raw.githubusercontent.com/example/public-source/{revision}/LICENSE", "file": "fixture-LICENSE", "sha256": hashlib.sha256(text).hexdigest()}
        (licenses / "manifest.json").write_text(json.dumps({"schemaVersion": 1, "entries": [entry]}), encoding="utf-8")
        (package_root / ".cargo_vcs_info.json").write_text(json.dumps({"git": {"sha1": revision}, "path_in_vcs": "crate"}), encoding="utf-8")
        return root, {key: entry[key] for key in ("name", "version", "license", "repository")}, package_root

    def test_frozen_original_preserves_attribution_and_pinned_source_without_network(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root, package, source = self.frozen_fixture(Path(temporary))
            texts, provenance = frozen_notices(root, package, source)
            dependency = Dependency("Cargo", "fixture", "1.0", "MIT", package["repository"], texts, provenance=provenance)
            document = render([dependency])
            self.assertIn("Copyright Original Holder", document)
            self.assertIn(provenance[0]["source"], document)
            self.assertIn(provenance[0]["sha256"], document)
            self.assertNotIn(str(root), document)
            other_version = {**package, "version": "2.0"}
            self.assertEqual(frozen_notices(root, other_version, source), ({}, []))

    def test_frozen_notice_rejects_tampering_revision_mismatch_and_path_escape(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root, package, source = self.frozen_fixture(Path(temporary))
            license_file = root / "vendor/licenses/fixture-LICENSE"
            original = license_file.read_bytes()
            license_file.write_bytes(b"Tampered attribution\n")
            with self.assertRaises(RuntimeError):
                frozen_notices(root, package, source)
            license_file.write_bytes(original)
            vcs_file = source / ".cargo_vcs_info.json"
            original_vcs = vcs_file.read_text()
            vcs_file.write_text(json.dumps({"git": {"sha1": "b" * 40}, "path_in_vcs": "crate"}))
            with self.assertRaises(RuntimeError):
                frozen_notices(root, package, source)
            vcs_file.write_text(original_vcs)
            manifest_file = root / "vendor/licenses/manifest.json"
            manifest = json.loads(manifest_file.read_text())
            manifest["entries"][0]["file"] = "../private-user-file"
            manifest_file.write_text(json.dumps(manifest))
            with self.assertRaises(RuntimeError):
                frozen_notices(root, package, source)

    def test_deterministic_inventory_deduplicates_only_identical_license_texts(self) -> None:
        first = Dependency("npm", "one", "1.0", "MIT", "https://example.org/one", {"LICENSE": "Copyright One\nMIT text\n"})
        second = Dependency("Cargo", "two", "2.0", "MIT", "", {"LICENSE": "Copyright Two\nMIT text\n"})
        third = Dependency("npm", "three", "3.0", "MIT", "", first.texts.copy())
        self.assertEqual(render([first, second, third]), render([third, second, first]))
        document = render([first, second, third])
        self.assertEqual(document.count("Copyright One"), 1)
        self.assertEqual(document.count("Copyright Two"), 1)
        self.assertNotIn(str(ROOT), document)
        self.assertEqual(safe_repository("https://token:secret@example.org/private?secret=1"), "")
        self.assertEqual(safe_repository({"url": "git+https://github.com/public/source.git?token=bad"}), "https://github.com/public/source.git")

    def test_license_reader_refuses_files_outside_package_and_keeps_attribution(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary)
            package = directory / "package"
            package.mkdir()
            (directory / "private").write_text("Private user data", encoding="utf-8")
            (package / "LICENSE").write_text("Copyright Holder\r\nPermission is granted.\r\n", encoding="utf-8")
            (package / "NOTICE").symlink_to(directory / "private")
            self.assertEqual(license_texts(package), {"LICENSE": "Copyright Holder\nPermission is granted.\n"})
            self.assertIsNone(read_license(directory / "private", package))

    def test_required_shipped_js_packages_fail_when_license_is_missing(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            for name in REQUIRED_JS:
                package = root / "node_modules" / name
                package.mkdir(parents=True)
                (package / "package.json").write_text(json.dumps({"name": name, "version": "1.0", "license": "MIT"}), encoding="utf-8")
                (package / "LICENSE").write_text("Copyright Holder\nMIT\n", encoding="utf-8")
            self.assertEqual(len(js_dependencies(root)), 2)
            (root / "node_modules" / "svelte" / "LICENSE").unlink()
            with self.assertRaises(RuntimeError):
                js_dependencies(root)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--check", action="store_true", help="Fail if the generated checked-in document differs")
    parser.add_argument("--output", type=Path, default=ROOT / "docs" / "THIRD_PARTY_NOTICES.md")
    parser.add_argument("--self-test", action="store_true", help="Run stdlib-only fixture tests without Cargo or npm")
    arguments = parser.parse_args()
    if arguments.self_test:
        suite = unittest.defaultTestLoader.loadTestsFromTestCase(GeneratorTests)
        return 0 if unittest.TextTestRunner(verbosity=2).run(suite).wasSuccessful() else 1
    try:
        dependencies = cargo_dependencies(ROOT) + js_dependencies(ROOT) + vendor_dependencies(ROOT)
        document = render(dependencies)
        if arguments.check:
            if not arguments.output.is_file() or arguments.output.read_text(encoding="utf-8") != document:
                print("Third-party notices are out of date; regenerate after installing locked dependencies", file=sys.stderr)
                return 1
        else:
            arguments.output.parent.mkdir(parents=True, exist_ok=True)
            arguments.output.write_text(document, encoding="utf-8")
        warnings = sum(len(dependency.warnings) for dependency in dependencies)
        print(f"Third-party notices: {len(dependencies)} packages, {warnings} source inventory warnings, {'verified' if arguments.check else 'generated'}")
        return 0
    except (RuntimeError, OSError, UnicodeError, KeyError, ValueError) as error:
        print(f"Third-party notices failed: {type(error).__name__}: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
