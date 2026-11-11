"""Conversation-round selection for deterministic fixture providers."""


def current_input(messages):
    previous_reply = next((index for index in range(len(messages) - 1, -1, -1)
                           if messages[index].get('role') == 'assistant'
                           and not messages[index].get('tool_calls')), -1)
    return next((index for index in range(previous_reply + 1, len(messages))
                 if messages[index].get('role') == 'user'), -1)
