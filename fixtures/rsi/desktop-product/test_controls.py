"""Native gestures start only after readiness and are never replayed."""
import unittest
from pathlib import Path
import tempfile
from unittest.mock import Mock
from controls import FOCUS_INPUT, fill, file_has_bytes, configure_live_limits


class InputControlTest(unittest.TestCase):
    def test_live_limits_are_expanded_and_filled_before_apply(self):
        events = []
        limits = dict(context_window_tokens=1048576, default_output_reserve_tokens=4096, max_output_reserve_tokens=393216)
        configure_live_limits(lambda source: events.append(('expand', source)), lambda css, value: events.append(('fill', css, value)), limits)
        self.assertEqual(events[0][0], 'expand')
        self.assertIn('Model settings 1', events[0][1])
        self.assertEqual(events[1:], [('fill', f'[aria-label="{field} 1"]', str(value)) for field, value in zip(["Context window", "Default output tokens", "Max output tokens"], limits.values())])
    def test_created_empty_and_partial_files_are_not_completed_writes(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / 'result'
            expected = b'native-pty-ok'
            self.assertFalse(file_has_bytes(path, expected))
            for incomplete in [b'', expected[:4], expected + expected]:
                path.write_bytes(incomplete)
                self.assertFalse(file_has_bytes(path, expected))
            path.write_bytes(expected)
            self.assertTrue(file_has_bytes(path, expected))

    def test_focus_wait_precedes_one_native_replacement(self):
        item = {'element-6066': 'deployment'}
        value = 'previous'
        focused = False
        keys = Mock()
        def script(source, arguments):
            if source == FOCUS_INPUT:
                if arguments[-1] == 'value':
                    return value
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

    def test_rendered_replacements_are_reacquired_without_replaying_keys(self):
        item = {'element-6066': 'before'}
        original = item
        value = 'previous'
        calls = []
        def script(source, arguments):
            if source == FOCUS_INPUT:
                if arguments[-1] == 'value':
                    return value
                return item
            if arguments == ['input']:
                return value
            if arguments != [item]:
                raise RuntimeError('stale element reference')
            if source.startswith('return {value:'):
                return {'value': value, 'events': 0}
            return value
        def keys(target, text):
            nonlocal item, value
            self.assertEqual(target, item)
            calls.append((target, text))
            value = '' if text.startswith('\ue009') else text
            item = {'element-6066': f'render-{len(calls)}'}
        fill(script, keys, lambda read: read(), 'input', 'new value')
        self.assertEqual(calls, [(original, '\ue009a\ue000\ue003'), ({'element-6066':'render-1'}, 'new value')])

    def test_ambiguous_native_input_failure_is_not_replayed(self):
        item = {'element-6066': 'deployment'}
        script = Mock(side_effect=[item, {'value': 'before', 'events': 0}])
        keys = Mock(side_effect=RuntimeError('native input outcome unknown'))
        with self.assertRaisesRegex(RuntimeError, 'outcome unknown'):
            fill(script, keys, lambda read: read(), 'input', 'value')
        keys.assert_called_once_with(item, '\ue009a\ue000\ue003')

    def test_stale_pre_input_handles_are_reacquired_without_replaying_keys(self):
        for stage in ['focus', 'state']:
            with self.subTest(stage=stage):
                item = {'element-6066': 'current'}
                value = 'before'
                attempts = 0
                calls = []
                def script(source, arguments):
                    nonlocal attempts
                    if source == FOCUS_INPUT and arguments[-1] == 'value':
                        return value
                    current = 'focus' if source == FOCUS_INPUT else 'state'
                    if current == stage:
                        attempts += 1
                        if attempts % 2:
                            raise RuntimeError({'value': {'error': 'stale element reference', 'message': 'Stale element found when trying to create the node handle'}})
                    return item if current == 'focus' else {'value': value, 'events': 0}
                def keys(target, text):
                    nonlocal value
                    self.assertEqual(target, item)
                    calls.append(text)
                    value = '' if text.startswith('\ue009') else text
                def until(read):
                    for _ in range(5):
                        result = read()
                        if result:
                            return result
                    raise TimeoutError('input never became ready')
                fill(script, keys, until, 'input', 'replacement')
                self.assertEqual(calls, ['\ue009a\ue000\ue003', 'replacement'])

    def test_other_readiness_errors_are_not_retried(self):
        error = RuntimeError({'value': {'error': 'javascript error', 'message': 'fixture failure'}})
        script, keys = Mock(side_effect=error), Mock()
        with self.assertRaises(RuntimeError) as caught:
            fill(script, keys, lambda read: read(), 'input', 'replacement')
        self.assertIs(caught.exception, error)
        script.assert_called_once()
        keys.assert_not_called()


if __name__ == '__main__':
    unittest.main()
