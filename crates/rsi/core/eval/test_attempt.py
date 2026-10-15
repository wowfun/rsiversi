import base64
import copy
import json
import unittest
from pathlib import Path

from attempt import RAW_BLOCK_BYTES, events, model_events


def log(text, reason="stop"):
    """Fixture encoder cross-checked by the shared Rust golden below."""

    def integer(value):
        result = bytearray()
        while value >= 128:
            result.append((value & 127) | 128)
            value >>= 7
        result.append(value)
        return result

    source = [
        (10, {"type": "content_started", "index": 0, "content": {"type": "text"}}),
        (
            12,
            {
                "type": "content_delta",
                "index": 0,
                "delta": {"type": "text", "value": text},
            },
        ),
        (20, {"type": "content_finished", "index": 0}),
        (22, {"type": "finished", "reason": reason, "replay": None}),
    ]
    raw = bytearray()
    previous = 0
    for elapsed, event in source:
        raw += integer(elapsed - previous)
        previous = elapsed
        if event["type"] == "content_delta":
            raw += b"\x01\x00"
            data = event["delta"]["value"].encode()
        else:
            raw.append(0)
            data = json.dumps(event, ensure_ascii=False, separators=(",", ":")).encode()
        raw += integer(len(data)) + data
    return {
        "v": 1,
        "retain_context": True,
        "chunks": [
            base64.b64encode(raw[i : i + RAW_BLOCK_BYTES]).decode()
            for i in range(0, len(raw), RAW_BLOCK_BYTES)
        ],
    }


class AttemptEvidence(unittest.TestCase):
    def test_rust_golden_preserves_unicode_clock_and_derived_coordinate(self):
        path = (
            Path(__file__).resolve().parents[3]
            / "rsi-agent/session-protocol/tests/fixtures/model-attempt-v1.json"
        )
        golden = json.loads(path.read_text(encoding="utf-8"))
        self.assertEqual(log("界🦀", "max_tokens"), golden)
        decoded = list(events(golden))
        self.assertEqual([time for time, _ in decoded], [10, 12, 20, 22])
        self.assertEqual(decoded[1][1]["delta"]["value"], "界🦀")
        fact = dict(
            type="model_attempt_settled",
            seq=19,
            turn_id="turn",
            effect_id="effect",
            purpose="conversation",
            attempt=golden,
        )
        before = copy.deepcopy(fact)
        records = list(model_events([fact]))
        self.assertEqual([record["seq"] for record in records], [19] * 4)
        self.assertEqual([record["event_index"] for record in records], list(range(4)))
        self.assertEqual(fact, before)

    def test_cross_block_events_preserve_exact_payload(self):
        text = "界🦀" * 40000
        decoded = list(events(log(text)))
        self.assertEqual(decoded[1][1]["delta"]["value"], text)

    def test_invalid_versions_blocks_and_records_fail_closed(self):
        valid = log("text")
        for change in [
            dict(v=2),
            dict(v=True),
            dict(chunks=[]),
            dict(chunks=["!"]),
            dict(chunks=[base64.b64encode(b"\x80\x00").decode()]),
            dict(chunks=[base64.b64encode(b"\x00\x04").decode()]),
        ]:
            with self.assertRaises(ValueError):
                list(events({**valid, **change}))
        for raw in [
            base64.b64decode(valid["chunks"][0])[:-1],
            base64.b64decode(valid["chunks"][0]) + b"\x00",
        ]:
            with self.assertRaises(ValueError):
                list(events({**valid, "chunks": [base64.b64encode(raw).decode()]}))


if __name__ == "__main__":
    unittest.main()
