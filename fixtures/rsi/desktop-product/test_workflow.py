"""Keyless Host-profile configuration checks; no native desktop prerequisites."""
import json
import os
from pathlib import Path
import tempfile
import tomllib
import unittest
from unittest.mock import Mock, patch

from workflow import configure, resource_button


class WorkflowControlTest(unittest.TestCase):
    def test_pending_detail_read_must_settle_before_one_native_click(self):
        control = {'element-6066-11e4-a52e-4f735466cecf': 'result'}
        script = Mock(side_effect=[None, control])
        click = Mock()

        def until(check):
            self.assertIsNone(check(), 'the pending detail has no enabled control')
            click.assert_not_called()
            return check()

        resource_button(script, click, until, 'Read result')
        click.assert_called_once_with(control)

    def test_missing_control_times_out_without_dispatch(self):
        script = Mock(return_value=None)
        click = Mock()

        def until(check):
            self.assertIsNone(check())
            raise TimeoutError('no ready control')

        with self.assertRaises(TimeoutError):
            resource_button(script, click, until, 'Read result')
        click.assert_not_called()


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
