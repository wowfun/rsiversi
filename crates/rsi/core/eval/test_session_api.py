import copy
import unittest
from unittest.mock import patch

from session_api import classify, grade_before_deadline, verify_counters, turn_budget, limits

DEFAULT = object()


class CompletionEvidence(unittest.TestCase):
    def setUp(self):
        self.request = dict(goal="goal", request_id="create", max_rounds=2, objective="Repair UTF-8", constraints="Check source")
        self.budget = dict(maximum_elapsed_ms=120000, maximum_provider_attempts=16, maximum_tool_calls=24, maximum_generated_records=65536, maximum_generated_record_bytes=67108864)
        self.execution = dict(exit_code=0, timed_out=False)
        self.reply = dict(transport="local_session_api", before_live=dict(armed=False), after_reads_live=dict(armed=False), live=dict(armed=False),
                          receipt=dict(command=dict(request_id="create")), goal=dict(goal=dict(
                              id="goal", objective=self.request["objective"], constraints=self.request["constraints"], turn_budget=copy.deepcopy(self.budget), max_rounds=2, allocated_rounds=2, phase="completed",
                              reservation=dict(round=2, settlement=dict(type="turn", turn_id="turn", outcome="completed")),
                              report=dict(kind="complete", source_turn="turn", evidence="checked"))))

    def grade(self, reply=DEFAULT, passed=True):
        return classify(self.execution, self.reply if reply is DEFAULT else reply, dict(passed=passed), self.request, self.budget)

    def test_frozen_inputs_cannot_be_changed_or_widened(self):
        for field in ("objective", "constraints", "turn_budget"):
            reply = copy.deepcopy(self.reply)
            reply["goal"]["goal"][field] = "different"
            self.assertEqual(self.grade(reply), "infrastructure_failure", field)
        for field, value in self.budget.items():
            reply = copy.deepcopy(self.reply)
            reply["goal"]["goal"]["turn_budget"][field] = value + 1
            self.assertEqual(self.grade(reply), "infrastructure_failure", field)
        for reply in [None, {}, "no evidence"]:
            self.assertEqual(self.grade(reply), "infrastructure_failure")

    def test_durable_counters_obey_per_turn_and_task_caps(self):
        task = {"prompts": ["task"]}
        budget = turn_budget(task)
        cap = limits(task, budget)
        for kind, field in [("model_started", "maximum_provider_attempts"), ("tool_started", "maximum_tool_calls")]:
            facts = [dict(type=kind, turn_id="one") for _ in range(budget[field])]
            verify_counters({"facts": facts}, budget, cap)
            with self.assertRaises(ValueError):
                verify_counters({"facts": facts + [facts[0]]}, budget, cap)
            facts += [dict(fact, turn_id="two") for fact in facts]
            verify_counters({"facts": facts}, budget, cap)
            with self.assertRaises(ValueError):
                verify_counters({"facts": facts + [dict(type=kind, turn_id="three")]}, budget, cap)

    def test_oracle_is_independent_of_model_completion(self):
        self.assertEqual(self.grade(), "passed")
        self.assertEqual(self.grade(passed=False), "behavioral_failure")

    def test_incomplete_cleanup_precedes_completion_and_timeout(self):
        for timed_out in (False, True):
            self.execution["timed_out"] = timed_out
            reply = copy.deepcopy(self.reply)
            reply["cleanup_errors"] = ["client shutdown deadline"]
            self.assertEqual(self.grade(reply), "infrastructure_failure")
        for timed_out in (False, True):
            for exit_code in (0, None):
                self.execution.update(cleanup_pending=True, timed_out=timed_out, exit_code=exit_code)
                self.assertEqual(self.grade(), "infrastructure_failure")

    def test_infrastructure_failure_precedes_timeout(self):
        self.execution["timed_out"] = True
        self.assertEqual(self.grade(), "budget_exhausted")
        for failure in ("launch_error", "output_limit_exceeded"):
            self.execution[failure] = True
            self.assertEqual(self.grade(), "infrastructure_failure")
            del self.execution[failure]
        self.assertEqual(classify(self.execution, self.reply, {"infrastructure_error": "oracle stopped"}, self.request, self.budget),
                         "infrastructure_failure")

    def test_grading_cannot_pass_after_the_task_deadline(self):
        with patch("session_api.time.monotonic", side_effect=[99, 101]), patch("session_api.coding.oracle", return_value={"passed": True}) as oracle:
            checked = grade_before_deadline("workspace", "task", 0, 100)
        oracle.assert_called_once_with("workspace", "task", 0, timeout_seconds=1)
        self.assertEqual(classify(self.execution, self.reply, checked, self.request, self.budget), "budget_exhausted")
        with patch("session_api.time.monotonic", return_value=100), patch("session_api.coding.oracle") as oracle:
            checked = grade_before_deadline("workspace", "task", 0, 100)
        oracle.assert_not_called()
        self.assertEqual(classify(self.execution, self.reply, checked, self.request, self.budget), "budget_exhausted")
        checked["infrastructure_error"] = "cleanup failed"
        self.assertEqual(classify(self.execution, self.reply, checked, self.request, self.budget), "infrastructure_failure")

    def test_malformed_or_wrong_identity_never_passes_or_throws(self):
        for path in [("before_live",), ("after_reads_live",), ("live",), ("receipt", "command"), ("goal",), ("goal", "goal"),
                     ("goal", "goal", "reservation"), ("goal", "goal", "report")]:
            for invalid in [None, [], "forged", {}, 0, True]:
                reply = copy.deepcopy(self.reply)
                parent = reply
                for name in path[:-1]:
                    parent = parent[name]
                parent[path[-1]] = invalid
                self.assertEqual(self.grade(reply), "infrastructure_failure", (path, invalid))
        self.request["request_id"] = "another-control"
        self.assertEqual(self.grade(), "infrastructure_failure")

    def test_report_needs_matching_successful_source_turn(self):
        goal = self.reply["goal"]["goal"]
        goal["report"]["source_turn"] = "different-turn"
        self.assertEqual(self.grade(), "infrastructure_failure")
        goal["report"]["source_turn"] = "turn"
        goal["reservation"]["settlement"]["outcome"] = "failed"
        self.assertEqual(self.grade(), "infrastructure_failure")

    def test_failure_at_cap_is_not_misclassified_as_exhaustion(self):
        goal = self.reply["goal"]["goal"]
        self.execution["exit_code"] = 2
        goal["phase"] = "blocked"
        goal["reservation"]["settlement"]["outcome"] = "failed"
        self.assertEqual(self.grade(), "agent_failure")
        goal["reservation"]["settlement"]["outcome"] = "budget_exceeded"
        self.assertEqual(self.grade(), "budget_exhausted")
        goal["reservation"]["settlement"]["outcome"] = "completed"
        goal["report"] = None
        self.assertEqual(self.grade(), "budget_exhausted")


if __name__ == "__main__":
    unittest.main()
