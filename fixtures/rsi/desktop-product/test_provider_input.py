"""Deterministic providers keep prompts and Tool rounds distinct from runtime context."""
import io
import unittest
from unittest.mock import Mock

from attention import provider_reply as attention
from external import provider_reply as external
from plan_review import provider_reply as plan_review
from provider_input import current_input
from tasks import ProviderControl
from typed_results import provider_reply as typed_results
from workflow import provider_reply as workflow
from workspace_review import reply as workspace_review


class ProviderInputTest(unittest.TestCase):
    def messages(self, prompt):
        return [{'role': 'user', 'content': 'earlier completed input'},
                {'role': 'assistant', 'content': 'earlier answer'},
                {'role': 'user', 'content': [{'type': 'text', 'text': prompt}]},
                {'role': 'user', 'content': 'Current time'},
                {'role': 'user', 'content': 'Current runtime context. Working directory: /fixture'}]

    def test_actual_routes_keep_current_input_and_do_not_repeat_tools(self):
        for route, prompt, expected in [
            (attention, 'desktop attention question', 'ask_user'),
            (external, 'start an external delegation', 'external_agent'),
            (plan_review, 'PLAN_REVIEW_DESKTOP', 'plan_write'),
            (typed_results, 'desktop typed profile catalog', 'host_profile'),
            (workflow, 'WORKFLOW_DESKTOP', 'run_workflow'),
            (workspace_review, 'REVIEW_PATCH', 'apply_patch'),
        ]:
            with self.subTest(tool=expected):
                messages = self.messages(prompt)
                reply = route({'messages': messages})
                self.assertEqual(reply['tool_calls'][0]['function']['name'], expected)
                messages += [{'role': 'assistant', 'tool_calls': reply['tool_calls']},
                             {'role': 'tool', 'tool_call_id': 'review', 'content': '{}'},
                             {'role': 'user', 'content': 'Updated runtime context'}]
                self.assertIsNone(route({'messages': messages}))

    def test_task_provider_continues_its_existing_tool_round(self):
        provider = ProviderControl()
        messages = self.messages('Native inline patch')
        handler = Mock(wfile=io.BytesIO())
        self.assertTrue(provider.handle(handler, {'messages': messages}))
        self.assertIn('apply_patch', handler.wfile.getvalue().decode())
        messages += [{'role': 'assistant', 'tool_calls': [{'id': 'patch'}]},
                     {'role': 'tool', 'tool_call_id': 'patch', 'content': '{}'},
                     {'role': 'user', 'content': 'Updated runtime context'}]
        handler = Mock(wfile=io.BytesIO())
        self.assertTrue(provider.handle(handler, {'messages': messages}))
        self.assertIn('Native task stream released.', handler.wfile.getvalue().decode())
        self.assertNotIn('tool_calls', handler.wfile.getvalue().decode())

    def test_no_current_input_after_completed_reply(self):
        self.assertEqual(current_input([{'role': 'user'}, {'role': 'assistant', 'content': 'done'}]), -1)


if __name__ == '__main__':
    unittest.main()
