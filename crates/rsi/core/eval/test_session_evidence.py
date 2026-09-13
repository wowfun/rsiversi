import copy
import unittest

from session_api import boundary_evidence, pressure_evidence, fact_chain
from session_fixture import BOUNDARY_MARKER, PRESSURE_TEXT, SUMMARY_FRAME, SUMMARY_TEXT


class PressureEvidence(unittest.TestCase):
    def setUp(self):
        self.trace = {"facts": [
            dict(type="model_intent", seq=10, effect_id="summary", turn_id="turn",
                 purpose=dict(kind="context_compaction", plan=dict(session="coding-session", sources=[dict(session="coding-session", turn="older", after_seq=0, through_seq=2, facts_sha256="pending")],
                              selections=[dict(turn="older", first=0, count=1)], view_sha256="source-view"))),
            dict(type="model_event", seq=11, effect_id="summary", turn_id="turn", purpose="context_compaction",
                 event=dict(type="content_delta", delta=dict(type="text", value=SUMMARY_TEXT))),
            dict(type="model_event", seq=12, effect_id="summary", turn_id="turn", purpose="context_compaction",
                 event=dict(type="finished", reason="stop")),
            dict(type="model_intent", seq=13, effect_id="ordinary", turn_id="turn", purpose=dict(kind="conversation"),
                 snapshot=dict(request_sha256="ordinary-request")),
        ]}
        source = [dict(type="model_event", seq=1, effect_id="answer", turn_id="older", purpose="conversation", event=dict(type="content_delta", delta=dict(type="text", value=PRESSURE_TEXT))),
                  dict(type="model_event", seq=2, effect_id="answer", turn_id="older", purpose="conversation", event=dict(type="finished", reason="stop"))]
        self.trace["facts"][0]["purpose"]["plan"]["sources"][0]["facts_sha256"] = fact_chain(source)
        self.trace["facts"] += source
        self.requests = [dict(stage="eval-goal-1", summary=True, messages=[dict(role="user", content=PRESSURE_TEXT)]),
                         dict(stage="eval-goal-1", summary=False, messages=[dict(role="system", content=SUMMARY_FRAME)])]

    def verify(self):
        return pressure_evidence(self.trace, self.requests, 1)

    def test_records_durable_source_output_and_later_wire_input(self):
        evidence = self.verify()[0]
        self.assertEqual((evidence["intent_seq"], evidence["finished_seq"], evidence["ordinary_seq"]), (10, 12, 13))
        self.assertEqual(len(evidence["ordinary_wire_messages_sha256"]), 64)

    def test_unrelated_selection_and_forged_source_digest_cannot_prove_removal(self):
        plan = self.trace["facts"][0]["purpose"]["plan"]
        for container, field, value in [(plan["sources"][0], "turn", "unrelated"), (plan["sources"][0], "facts_sha256", "f" * 64),
                                        (plan["selections"][0], "turn", "unrelated"), (plan["selections"][0], "first", 1)]:
            original = container[field]
            container[field] = value
            with self.assertRaises(ValueError):
                self.verify()
            container[field] = original

    def test_marker_echo_and_uncompacted_history_do_not_prove_installation(self):
        for message in [dict(role="assistant", content=SUMMARY_TEXT), dict(role="assistant", content=SUMMARY_FRAME),
                        dict(role="system", content=SUMMARY_TEXT)]:
            self.requests[1]["messages"] = [message]
            with self.assertRaises(ValueError):
                self.verify()
        self.requests[1]["messages"] = [dict(role="system", content=SUMMARY_FRAME), dict(role="assistant", content=PRESSURE_TEXT)]
        with self.assertRaises(ValueError):
            self.verify()

    def test_successful_finished_must_belong_to_the_same_prior_effect(self):
        finished = self.trace["facts"][2]
        for key, value in [("effect_id", "foreign"), ("turn_id", "foreign"), ("seq", 14)]:
            original = finished[key]
            finished[key] = value
            with self.assertRaises(ValueError):
                self.verify()
            finished[key] = original
        finished["event"]["reason"] = "cancelled"
        with self.assertRaises(ValueError):
            self.verify()

    def test_missing_stage_and_missing_ordinary_request_fail(self):
        with self.assertRaises(ValueError):
            pressure_evidence(self.trace, self.requests, 2)
        del self.trace["facts"][3]
        self.requests.pop()
        with self.assertRaises(ValueError):
            self.verify()


class BoundaryEvidence(unittest.TestCase):
    def test_requires_actual_successful_restricted_tool_result(self):
        trace = {"facts": [dict(type="tool_intent", seq=1, effect_id="probe", turn_id="turn", name="bash",
                                arguments=dict(command="probe")),
                           dict(type="tool_result", seq=2, effect_id="probe", turn_id="turn", result=dict(
                               is_error=False, value=dict(exit_code=0, stdout=dict(text=BOUNDARY_MARKER + "\n")),
                               enforcement=[dict(filesystem="workspace_write", scratch="private_tmp", backend=dict(kind="bubblewrap"))]))]}
        self.assertEqual(len(boundary_evidence(trace, "probe", 1)), 1)
        for field, value in [("enforcement", []), ("is_error", True), ("value", dict(exit_code=1))]:
            changed = copy.deepcopy(trace)
            changed["facts"][1]["result"][field] = value
            with self.assertRaises(ValueError):
                boundary_evidence(changed, "probe", 1)
        with self.assertRaises(ValueError):
            boundary_evidence(trace, "different command", 1)


if __name__ == "__main__":
    unittest.main()
