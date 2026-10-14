"""Read bounded compact attempt evidence without changing canonical Fact bytes."""

import base64
import json

RAW_BLOCK_BYTES = (192 * 1024 - 4) // 4 * 3
STREAM_BYTES = 32 * 1024 * 1024 - 4096


def events(attempt):
    if (
        not isinstance(attempt, dict)
        or set(attempt) != {"v", "retain_context", "chunks"}
        or type(attempt["v"]) is not int
        or attempt["v"] != 1
        or type(attempt["retain_context"]) is not bool
        or not isinstance(attempt["chunks"], list)
        or not attempt["chunks"]
    ):
        raise ValueError("invalid compact attempt envelope")
    size = 64
    blocks = []
    for index, chunk in enumerate(attempt["chunks"]):
        if not isinstance(chunk, str) or len(chunk) + 3 > 256 * 1024:
            raise ValueError("invalid compact attempt block")
        size += len(chunk) + 3
        if size > STREAM_BYTES:
            raise ValueError("compact attempt exceeds capacity")
        decoded = base64.b64decode(chunk, validate=True)
        if (
            not decoded
            or base64.b64encode(decoded).decode() != chunk
            or (index + 1 < len(attempt["chunks"]) and len(decoded) != RAW_BLOCK_BYTES)
        ):
            raise ValueError("noncanonical compact attempt block")
        blocks.append(decoded)
    data = b"".join(blocks)
    offset = 0

    def byte():
        nonlocal offset
        if offset >= len(data):
            raise ValueError("truncated attempt record")
        value = data[offset]
        offset += 1
        return value

    def varint():
        value = 0
        for shift in range(0, 70, 7):
            part = byte()
            if shift == 63 and part > 1:
                raise ValueError("attempt integer overflow")
            value |= (part & 127) << shift
            if part < 128:
                if shift and not part:
                    raise ValueError("noncanonical attempt integer")
                return value
        raise ValueError("invalid attempt integer")

    elapsed = 0
    terminal = False
    while offset < len(data):
        if terminal:
            raise ValueError("event follows attempt terminal")
        elapsed += varint()
        if elapsed > 2**64 - 1:
            raise ValueError("attempt clock overflow")
        tag = byte()
        if tag not in (0, 1, 2, 3):
            raise ValueError("unknown attempt tag")
        index = varint() if tag else 0
        length = varint()
        if index > 2**32 - 1 or length > STREAM_BYTES or offset + length > len(data):
            raise ValueError("invalid attempt record length or index")
        payload = data[offset : offset + length].decode("utf-8")
        offset += length
        if tag:
            event = {
                "type": "content_delta",
                "index": index,
                "delta": {
                    "type": ("text", "reasoning", "tool_arguments")[tag - 1],
                    "value": payload,
                },
            }
        else:
            event = json.loads(payload)
            if (
                not isinstance(event, dict)
                or event.get("type")
                not in {
                    "content_started",
                    "content_finished",
                    "usage",
                    "source",
                    "warning",
                    "finished",
                    "failed",
                }
                or json.dumps(event, ensure_ascii=False, separators=(",", ":"))
                != payload
            ):
                raise ValueError("noncanonical attempt event")
        terminal = event["type"] in ("finished", "failed")
        yield elapsed, event
    if not terminal:
        raise ValueError("attempt has no terminal")


def model_events(facts):
    """Derived coordinates retain seq; they never participate in Fact hashing."""
    for fact in facts:
        if fact.get("type") != "model_attempt_settled":
            continue
        for index, (elapsed, event) in enumerate(events(fact["attempt"])):
            yield {
                "type": "attempt_event",
                "seq": fact["seq"],
                "turn_id": fact["turn_id"],
                "effect_id": fact["effect_id"],
                "purpose": fact["purpose"],
                "event_index": index,
                "elapsed_ms": elapsed,
                "event": event,
            }
