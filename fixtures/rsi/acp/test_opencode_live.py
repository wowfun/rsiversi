import json
import os
from pathlib import Path
import runpy
import tempfile
import unittest
from unittest.mock import patch


class ChildReached(Exception):
    pass


class OpenCodeLaunchTests(unittest.TestCase):
    def test_explicit_bundle_replaces_inherited_artifact_selectors(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            auth = root / 'auth.json'
            auth.write_text(json.dumps({'opencode': {'key': 'synthetic'}}), encoding='utf-8')
            bundle = root / 'bundle'
            bundle.mkdir()

            def child(command, *, env, **kwargs):
                self.assertEqual(command[:2], ['node', 'fixtures/rsi/acp/opencode-browser.mjs'])
                self.assertEqual(env['RSI_PAIRED_BUNDLE'], str(bundle.resolve()))
                self.assertNotIn('RSI_WEB_BINARY', env)
                self.assertNotIn('RSI_WEB_ASSETS', env)
                self.assertTrue(Path(env['RSI_OPENCODE_FIXTURE']).is_file())
                raise ChildReached

            arguments = ['opencode-live.py', '--auth', str(auth), '--opencode', str(root / 'opencode'),
                         '--bundle', str(bundle), '--report', str(root / 'report')]
            with patch('sys.argv', arguments), patch.dict(os.environ, {
                'RSI_WEB_BINARY': 'stale-binary', 'RSI_WEB_ASSETS': 'stale-assets', 'RSI_PAIRED_BUNDLE': 'stale-bundle'
            }), patch('subprocess.check_output', return_value='synthetic-version'), patch('subprocess.run', side_effect=child):
                with self.assertRaises(ChildReached):
                    runpy.run_path(str(Path(__file__).with_name('opencode-live.py')), run_name='__main__')
