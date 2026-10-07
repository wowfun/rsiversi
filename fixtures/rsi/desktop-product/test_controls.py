"""Native gestures start only after readiness and are never replayed."""
import unittest
from unittest.mock import Mock
from controls import FOCUS_INPUT, fill


class InputControlTest(unittest.TestCase):
    def test_focus_wait_precedes_one_native_replacement(self):
        item = {'element-6066': 'deployment'}
        value = 'previous'
        focused = False
        keys = Mock()
        def script(source, arguments):
            if source == FOCUS_INPUT:
                return item if focused else None
            if source.startswith('return {value:'):
                return {'value': value, 'events': 0}
            return value
        def until(read):
            nonlocal focused
            if not focused:
                self.assertIsNone(read())
                keys.assert_not_called()
                focused = True
            return read()
        def type_keys(_item, text):
            nonlocal value
            value = '' if text.startswith('\ue009') else text
        keys.side_effect = type_keys
        fill(script, keys, until, '[aria-label="Deployment name"]', 'desktop-provider')
        self.assertEqual([call.args for call in keys.call_args_list], [(item, '\ue009a\ue000\ue003'), (item, 'desktop-provider')])

    def test_focus_timeout_does_not_type_or_schedule_replay(self):
        keys = Mock()
        def until(read):
            self.assertIsNone(read())
            raise TimeoutError('focus unavailable')
        with self.assertRaises(TimeoutError):
            fill(Mock(return_value=None), keys, until, 'input', 'value')
        keys.assert_not_called()

    def test_ambiguous_native_input_failure_is_not_replayed(self):
        item = {'element-6066': 'deployment'}
        script = Mock(side_effect=[item, {'value': 'before', 'events': 0}])
        keys = Mock(side_effect=RuntimeError('native input outcome unknown'))
        with self.assertRaisesRegex(RuntimeError, 'outcome unknown'):
            fill(script, keys, lambda read: read(), 'input', 'value')
        keys.assert_called_once_with(item, '\ue009a\ue000\ue003')


if __name__ == '__main__':
    unittest.main()
