#!/usr/bin/env python3
"""Exercise the exact release workflow helpers with isolated, offline HTTP fakes."""

import argparse
import ast
import hashlib
import io
import re
import tempfile
import textwrap
import traceback
import types
import unittest
import urllib.error
import urllib.parse
import urllib.request
from pathlib import Path


WORKFLOW = Path(__file__).resolve().parents[1] / '.github/workflows/release.yml'
JSON_ACCEPT = 'application/vnd.github+json'
BINARY_ACCEPT = 'application/octet-stream'
TOKEN = 'synthetic-test-token-never-real'
CDN = 'https://objects.githubusercontent.com/synthetic?signature=private-fixture'
PAYLOAD = b'bounded release fixture\n'


def workflow_tree(path):
    text = path.read_text(encoding='utf-8')
    marker = "cat > release-transport/release.py <<'PY'"
    _, separator, body = text.partition(marker)
    if not separator:
        raise AssertionError('Release helper heredoc is missing')
    lines = body.splitlines()[1:]
    end = next(index for index, line in enumerate(lines) if line.strip() == 'PY')
    return ast.parse(textwrap.dedent('\n'.join(lines[:end])), filename='release-workflow-helper')


class Response(io.BytesIO):
    def __init__(self, status, body=b'', location=None):
        super().__init__(body)
        self.status = status
        self.headers = {} if location is None else {'Location': location}


class DownloadTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.target = Path(self.directory.name) / 'download.zip'
        self.digest = hashlib.sha256(PAYLOAD).hexdigest()
        self.tree = workflow_tree(WORKFLOW)
        self.calls = []
        self.responses = []
        self.failure = None
        self.artifact_accept_required = False

        class Opener:
            def open(inner, request, timeout):
                headers = {key.lower(): value for key, value in request.header_items()}
                self.calls.append((request.full_url, headers, timeout))
                if self.failure is not None:
                    raise self.failure
                if self.artifact_accept_required and '/actions/artifacts/' in request.full_url:
                    if headers.get('accept') != JSON_ACCEPT:
                        raise urllib.error.HTTPError(request.full_url, 415, 'Unsupported', {}, None)
                response = self.responses.pop(0)
                if response.status in (301, 302, 303, 307, 308):
                    raise urllib.error.HTTPError(
                        request.full_url, response.status, 'Redirect', response.headers, response
                    )
                return response

        isolated_urllib = types.SimpleNamespace(
            error=urllib.error,
            parse=urllib.parse,
            request=types.SimpleNamespace(
                HTTPRedirectHandler=urllib.request.HTTPRedirectHandler,
                Request=urllib.request.Request,
                build_opener=lambda *handlers: Opener(),
            ),
        )
        self.namespace = {
            'hashlib': hashlib, 're': re, 'urllib': isolated_urllib,
            'TOKEN': TOKEN, 'LIMIT': 1024 * 1024 * 1024,
            'API': 'https://api.github.com/repos/example/library-manager',
        }
        names = {'require', 'file_sha', 'NoRedirect', 'request', 'download'}
        definitions = [node for node in self.tree.body
                       if isinstance(node, (ast.FunctionDef, ast.ClassDef)) and node.name in names]
        self.assertEqual({node.name for node in definitions}, names)
        # Execute definitions only: no imports, environment reads or release entry point.
        module = ast.Module(body=definitions, type_ignores=[])
        exec(compile(module, 'release-workflow-helper', 'exec'), self.namespace)

    def call_from_workflow(self, fragment, bindings):
        candidates = [node for node in ast.walk(self.tree)
                      if isinstance(node, ast.Call) and isinstance(node.func, ast.Name)
                      and node.func.id == 'download'
                      and any(isinstance(part, ast.Constant) and isinstance(part.value, str)
                              and fragment in part.value for part in ast.walk(node.args[0]))]
        self.assertTrue(candidates, 'Actual workflow download call is missing')
        namespace = dict(self.namespace, **bindings)
        return eval(compile(ast.Expression(candidates[0]), 'release-workflow-call', 'eval'), namespace)

    def test_artifact_api_call_uses_github_json_then_binary_cdn(self):
        self.artifact_accept_required = True
        self.responses = [Response(302, location=CDN), Response(200, PAYLOAD)]
        result = self.call_from_workflow('/actions/artifacts/', {
            'artifact': {'id': 7}, 'archive': self.target, 'digest': 'sha256:' + self.digest,
        })
        self.assertEqual(result, self.digest)
        self.assertEqual(self.target.read_bytes(), PAYLOAD)
        self.assertEqual(self.calls[0][1]['accept'], JSON_ACCEPT)
        self.assertEqual(self.calls[0][1]['authorization'], 'Bearer ' + TOKEN)
        self.assertEqual(self.calls[1][1]['accept'], BINARY_ACCEPT)
        self.assertNotIn('authorization', self.calls[1][1])
        self.assertEqual([call[2] for call in self.calls], [120, 120])

    def test_release_asset_call_defaults_to_binary(self):
        self.responses = [Response(200, PAYLOAD)]
        result = self.call_from_workflow('/releases/assets/', {
            'asset': {'id': 8, 'name': self.target.name},
            'verify_dir': self.target.parent, 'expected': {self.target.name: self.digest},
        })
        self.assertEqual(result, self.digest)
        self.assertEqual(self.calls[0][1]['accept'], BINARY_ACCEPT)
        self.assertEqual(self.target.read_bytes(), PAYLOAD)

    def test_redirect_forces_binary_and_drops_authentication(self):
        self.responses = [Response(302, location=CDN), Response(200, PAYLOAD)]
        self.namespace['download'](
            self.namespace['API'] + '/synthetic', self.target, self.digest, accept=JSON_ACCEPT
        )
        self.assertEqual(self.calls[0][1]['accept'], JSON_ACCEPT)
        self.assertEqual(self.calls[1][1]['accept'], BINARY_ACCEPT)
        self.assertNotIn('authorization', self.calls[1][1])

    def test_anonymous_public_download_does_not_add_a_token(self):
        self.responses = [Response(200, PAYLOAD)]
        self.namespace['download'](CDN, self.target, self.digest, auth=False)
        self.assertEqual(self.calls[0][1]['accept'], BINARY_ACCEPT)
        self.assertNotIn('authorization', self.calls[0][1])

    def test_digest_mismatch_refuses_the_download(self):
        self.responses = [Response(200, PAYLOAD)]
        with self.assertRaisesRegex(RuntimeError, '^Download digest mismatch$'):
            self.namespace['download'](self.namespace['API'] + '/synthetic', self.target, '0' * 64)

    def test_malicious_redirects_are_not_followed(self):
        for location in ['https://githubusercontent.com.evil.invalid/file',
                         'https://evil.invalid/file', 'http://objects.githubusercontent.com/file']:
            with self.subTest(location=location):
                self.calls.clear()
                self.responses = [Response(302, location=location)]
                with self.assertRaises(RuntimeError):
                    self.namespace['download'](self.namespace['API'] + '/synthetic', self.target)
                self.assertEqual(len(self.calls), 1)
                self.assertFalse(self.target.exists())

    def test_request_failures_hide_token_and_signed_url(self):
        failures = [urllib.error.HTTPError(CDN, 415, TOKEN, {}, None),
                    urllib.error.URLError(TOKEN + ' ' + CDN)]
        for failure in failures:
            with self.subTest(error=type(failure).__name__):
                self.failure = failure
                try:
                    self.namespace['download'](CDN, self.target, auth=False)
                except RuntimeError as error:
                    rendered = ''.join(traceback.format_exception(type(error), error, error.__traceback__))
                    self.assertTrue(error.__suppress_context__)
                    self.assertNotIn(TOKEN, rendered)
                    self.assertNotIn(CDN, rendered)
                    self.assertNotIn('signature=private-fixture', rendered)
                else:
                    self.fail('Request failure was accepted')
                self.assertFalse(self.target.exists())


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--workflow', type=Path, default=WORKFLOW)
    options, remaining = parser.parse_known_args()
    WORKFLOW = options.workflow
    unittest.main(argv=[__file__, *remaining])
