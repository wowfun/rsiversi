from pathlib import Path
import tempfile
import unittest
from live import read_deepseek_key, live_limits


class LiveCredentialTests(unittest.TestCase):
    def test_explicit_file_bound_and_line_boundaries(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / 'synthetic.env'
            for line in ['DEEPSEEK_API_KEY=synthetic', ' export DEEPSEEK_API_KEY="synthetic"', "DEEPSEEK_API_KEY='synthetic'"]:
                path.write_bytes((line + '\r\n').encode())
                self.assertEqual(read_deepseek_key(path), 'synthetic')
            for text in ['OTHER_KEY=synthetic', 'DEEPSEEK_API_KEY=""', 'DEEPSEEK_API_KEY="   "', 'DEEPSEEK_API_KEY=\nOTHER_KEY=synthetic', 'x' * 65537]:
                with self.subTest(text=text[:40]):
                    path.write_text(text, encoding='utf-8')
                    with self.assertRaisesRegex(ValueError, 'absent|empty|bound'):
                        read_deepseek_key(path)

    def test_explicit_limits_are_bounded_and_not_inferred(self):
        env = {'DEEPSEEK_CONTEXT_WINDOW_TOKENS':'1048576', 'DEEPSEEK_DEFAULT_OUTPUT_RESERVE_TOKENS':'4096', 'DEEPSEEK_MAX_OUTPUT_RESERVE_TOKENS':'393216'}
        self.assertEqual(live_limits(env)['max_output_reserve_tokens'], 393216)
        for invalid in [None, '', '0', '1.5', '1e6', '4294967296', ' 4096', '1'*100]:
            with self.subTest(value=invalid), self.assertRaisesRegex(ValueError, 'DEEPSEEK_CONTEXT_WINDOW_TOKENS'):
                live_limits(dict(env, DEEPSEEK_CONTEXT_WINDOW_TOKENS=invalid))
        with self.assertRaisesRegex(ValueError, 'maximum output reserve'):
            live_limits(dict(env, DEEPSEEK_MAX_OUTPUT_RESERVE_TOKENS='4095'))
