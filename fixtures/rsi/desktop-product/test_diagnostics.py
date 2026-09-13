import contextlib
import io
import json
from pathlib import Path
import tempfile
import unittest
import subprocess
import sys
from diagnostics import record_failure, redact_evidence, run_cleanup


class DiagnosticsTest(unittest.TestCase):
    def test_cleanup_runs_every_phase_and_preserves_a_primary_failure(self):
        for primary in [None, RuntimeError('primary product failure')]:
            calls = []
            def action(name, fail):
                calls.append(name)
                if fail:
                    raise RuntimeError(name + ' failed')
            steps = [(name, lambda name=name, fail=fail: action(name, fail))
                     for name, fail in [('stop', True), ('wait', True), ('close log', False), ('remove runtime', False)]]
            if primary:
                run_cleanup(steps, primary)
                self.assertEqual(str(primary), 'primary product failure')
                self.assertEqual(len(primary.__notes__), 2)
            else:
                with self.assertRaises(ExceptionGroup) as caught:
                    run_cleanup(steps)
                self.assertEqual(len(caught.exception.exceptions), 2)
            self.assertEqual(calls, ['stop', 'wait', 'close log', 'remove runtime'])

    def test_json_escaped_credentials_are_redacted_before_and_after_serialization(self):
        secret = 'key"\\\t雪'
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            for ascii_only in (True, False):
                path = root / 'escaped.json'
                path.write_text(json.dumps({'nested': [secret]}, ensure_ascii=ascii_only))
                with self.assertRaisesRegex(RuntimeError, 'credential'):
                    redact_evidence(root, secret)
                self.assertEqual(json.loads(path.read_text()), {'nested': ['[REDACTED]']})
            with contextlib.redirect_stderr(io.StringIO()):
                record_failure(root, 'failure', RuntimeError(secret), None, None, secret)
            self.assertEqual(json.loads((root / 'failure.json').read_text())['error'], '[REDACTED]')

    def test_redaction_never_follows_a_symlink_outside_its_evidence_directory(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            outside = root / 'outside.txt'
            outside.write_text('secret')
            evidence = root / 'evidence'
            evidence.mkdir()
            (evidence / 'linked.log').symlink_to(outside)
            (evidence / 'regular.log').write_text('secret')
            with self.assertRaisesRegex(RuntimeError, 'symlink'):
                redact_evidence(evidence, 'secret')
            self.assertEqual(outside.read_text(), 'secret')
            self.assertEqual((evidence / 'regular.log').read_text(), '[REDACTED]')
    def test_successful_run_still_fails_after_redacting_every_text_file(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            for name in ('one.log', 'two.json', 'three.html'):
                (root / name).write_text('test-secret')
            with self.assertRaisesRegex(RuntimeError, 'credential'):
                redact_evidence(root, 'test-secret')
            self.assertTrue(all(path.read_text() == '[REDACTED]' for path in root.iterdir()))

    def test_early_exit_records_phase_code_and_bounded_redacted_tail(self):
        class Exited:
            @staticmethod
            def poll():
                return 7

        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / 'daemon.log').write_text('omitted-prefix' + 'x' * 70000 + 'secret-value\n')
            output = io.StringIO()
            with contextlib.redirect_stderr(output):
                record_failure(root, 'paired-daemon-startup', RuntimeError('secret-value'), Exited(), None, 'secret-value')
            failure = json.loads((root / 'failure.json').read_text())
            self.assertEqual(failure['daemonExitCode'], 7)
            self.assertEqual(failure['phase'], 'paired-daemon-startup')
            self.assertIsNone(failure['webdriverExitCode'])
            self.assertNotIn('secret-value', output.getvalue())
            self.assertNotIn('omitted-prefix', output.getvalue())
            self.assertIn('[REDACTED]', output.getvalue())
            self.assertLess(len(output.getvalue()), 66000)

    def test_spawn_failure_needs_no_process_or_log(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            with contextlib.redirect_stderr(io.StringIO()):
                record_failure(root, 'webdriver-startup', FileNotFoundError('driver'), None, None)
            failure = json.loads((root / 'failure.json').read_text())
            self.assertIsNone(failure['daemonExitCode'])
            self.assertEqual(failure['error'], 'driver')

    def test_live_redaction_preserves_the_original_product_failure(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            driver = root / 'driver'
            driver.write_text('''#!/usr/bin/env python3
import json, os, sys
from http.server import BaseHTTPRequestHandler, HTTPServer
class Handler(BaseHTTPRequestHandler):
    def log_message(self, *_): pass
    def do_GET(self):
        self.send_response(200); self.end_headers()
        self.wfile.write(b'{"value":{"ready":true}}')
    def do_POST(self):
        self.send_response(500); self.end_headers()
        self.wfile.write(b'{"value":{"message":"fixture primary failure"}}')
print(os.environ['DEEPSEEK_API_KEY'], flush=True)
port = int(next(arg.split('=')[1] for arg in sys.argv if arg.startswith('--port=')))
HTTPServer(('127.0.0.1', port), Handler).serve_forever()
''')
            driver.chmod(0o700)
            env_file = root / 'env'
            env_file.write_text('DEEPSEEK_API_KEY=isolated-redaction-test-secret\n')
            report = root / 'report'
            result = subprocess.run([
                sys.executable, str(Path(__file__).with_name('verify.py')),
                '--binary', str(root / 'unused-binary'), '--assets', str(root),
                '--driver', str(driver), '--report', str(report),
                '--live-env-file', str(env_file), '--live-model', 'unused-model',
            ], capture_output=True, text=True, timeout=10)
            self.assertNotEqual(result.returncode, 0)
            failure = json.loads((report / 'failure.json').read_text())
            self.assertIn('fixture primary failure', failure['error'])
            # The child wrote directly through the still-open parent's log descriptor.
            self.assertIn('[REDACTED]', result.stderr)
            self.assertNotIn('isolated-redaction-test-secret', result.stderr)
            self.assertNotIn('isolated-redaction-test-secret', (report / 'webdriver.log').read_text())
            exception_lines = [line for line in result.stderr.splitlines() if line.startswith('RuntimeError:')]
            self.assertIn('fixture primary failure', exception_lines[-1])


if __name__ == '__main__':
    unittest.main()
