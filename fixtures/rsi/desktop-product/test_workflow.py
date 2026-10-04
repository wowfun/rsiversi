"""Keyless Host-profile configuration checks; no native desktop prerequisites."""
import json
import os
from pathlib import Path
import tempfile
import tomllib
import unittest
from unittest.mock import patch

from workflow import configure


class WorkflowConfigurationTest(unittest.TestCase):
    def test_empty_steps_whitespace_produces_one_valid_host_patch(self):
        with tempfile.TemporaryDirectory() as directory:
            config = Path(directory)
            host = config / 'host.profile.toml'
            for source in ['steps = []\n', 'steps = [] \n', 'steps\t=\t[]\t\n', '[[steps]]\nkind="include"\npath="base.profile.toml"\n']:
                with self.subTest(source=source):
                    host.write_text(source)
                    (config / 'settings.json').write_text('{}')
                    with patch.dict(os.environ, {'RSI_TEST_NODE': '/fixture/node'}):
                        configure(config, host)
                    steps = tomllib.loads(host.read_text())['steps']
                    patches = [step for step in steps if step['kind'] == 'patch']
                    self.assertEqual(patches, [
                        {'kind': 'patch', 'target': 'program-runtime', 'config_json': json.dumps({'node': '/fixture/node'})},
                        {'kind': 'patch', 'target': 'program-runtime', 'enabled': True},
                    ])
                    self.assertEqual(json.loads((config / 'settings.json').read_text())['rsi.agent-presets'], {'default': 'workflow'})


if __name__ == '__main__':
    unittest.main()
