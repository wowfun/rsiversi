"""Native Goal/patch/Jobs mechanism checks, with explicit provider response gates."""
import json
import re
import threading


class ProviderControl:
    def __init__(self):
        self.lock = threading.Lock()
        self.held = {}
        self.sequence = 0

    def handle(self, handler, body):
        messages = body.get('messages', [])
        index = next((i for i in range(len(messages) - 1, -1, -1) if messages[i]['role'] == 'user'), -1)
        content = messages[index].get('content', '') if index >= 0 else ''
        prompt = content if isinstance(content, str) else '\n'.join(item.get('text', '') for item in content)
        if not any(marker in prompt for marker in ('Native inline patch', 'Native Goal hold', 'Native background job')):
            return False
        completed = any(message['role'] == 'tool' for message in messages[index + 1:])
        with self.lock:
            self.sequence += 1
            sequence = self.sequence
        handler.send_response(200)
        handler.send_header('Content-Type', 'text/event-stream')
        handler.end_headers()
        def event(delta, reason=None):
            handler.wfile.write(('data: ' + json.dumps({'choices': [{'delta': delta, 'finish_reason': reason}]}) + '\n\n').encode())
            handler.wfile.flush()
        if 'Native Goal hold' in prompt or ('Native background job' in prompt and completed):
            released = threading.Event()
            with self.lock:
                self.held[sequence] = (prompt, released)
            try:
                event({'content': 'Waiting for native fixture release.'})
                if not released.wait(60):
                    raise TimeoutError('native provider gate was not released')
            finally:
                with self.lock:
                    self.held.pop(sequence, None)
        elif not completed:
            if 'Native inline patch' in prompt:
                name, arguments = 'apply_patch', {'patch': '*** Begin Patch\n*** Update File: card.txt\n@@\n-before\n+after · 界\n*** End Patch\n'}
            else:
                name, arguments = 'bash', {'command': "printf 'job-ready\\n'; while [ ! -f job-release ]; do sleep 0.05; done; printf 'job-done\\n'", 'run_in_background': True}
            event({'tool_calls': [{'index': 0, 'id': f'native-call-{sequence}', 'type': 'function',
                                  'function': {'name': name, 'arguments': json.dumps(arguments)}}]})
            event({}, 'tool_calls')
            handler.wfile.write(b'data: [DONE]\n\n')
            return True
        event({'content': 'Native task stream released.'})
        event({}, 'stop')
        handler.wfile.write(b'data: [DONE]\n\n')
        return True

    def release(self, marker):
        with self.lock:
            matches = [event for prompt, event in self.held.values() if marker in prompt]
        assert matches, f'no native provider gate for {marker}'
        for event in matches:
            event.set()

    def close(self):
        with self.lock:
            for _, event in self.held.values():
                event.set()


def verify(script, button, fill, until, screenshot, workspace, report, provider):
    button('Trajectory')
    def text(selector):
        return script('return document.querySelector(arguments[0])?.innerText??""', [selector])
    def has(value):
        return value in text('#detail .ui-contribution')
    def close():
        button('Close details')
        until(lambda: script('return !document.querySelector("#detail").open'))
    def send(prompt, expected):
        fill('textarea[aria-label="Main message"]', prompt)
        button('Send ↗')
        def ready():
            pending = script('return [...document.querySelectorAll(".pending button")].find(b=>b.textContent.startsWith("Review:"))?.textContent??null')
            if pending:
                button(pending)
                button('Allow once')
            return expected in text('.transcript')
        until(ready)
    (workspace / 'card.txt').write_text('before\n')
    send('Native inline patch', 'Native task stream released.')
    until(lambda: '+after · 界' in text('.inline-card:not([hidden])'))
    assert (workspace / 'card.txt').read_text() == 'after · 界\n'
    (workspace / 'card.txt').write_text('later unrelated edit\n')
    assert '-before' in text('.inline-card:not([hidden])') and 'later unrelated edit' not in text('.inline-card:not([hidden])')
    script('document.querySelector(".inline-card:not([hidden]) pre").scrollIntoView({block:"center"})')
    inline_geometry = until(lambda: script('''
        const diff=document.querySelector('.inline-card:not([hidden]) pre'), box=diff.getBoundingClientRect();
        if(box.width<=0||box.height<=0||box.top<0||box.bottom>innerHeight||box.left<0||box.right>innerWidth)return null;
        for(let parent=diff.parentElement;parent;parent=parent.parentElement){
            const style=getComputedStyle(parent),clip=parent.getBoundingClientRect();
            if(/(auto|scroll|hidden|clip)/.test(style.overflowY)&&(box.top<clip.top||box.bottom>clip.bottom))return null;
            if(/(auto|scroll|hidden|clip)/.test(style.overflowX)&&(box.left<clip.left||box.right>clip.right))return null;
        }
        const hit=diff.contains(document.elementFromPoint(box.x+box.width/2,box.y+box.height/2));
        return hit?{top:box.top,bottom:box.bottom,left:box.left,right:box.right,hit}:null;
    '''))
    screenshot('tasks-inline.png')
    button('Goal')
    fill('[aria-label="Goal objective"]', 'Native Goal hold for control evidence')
    fill('[aria-label="Maximum automatic rounds"]', '3')
    button('Create and start Goal')
    until(lambda: has('Allocated rounds: 1 / 3') and has('Current driving: Armed'))
    until(lambda: 'Waiting for native fixture release.' in text('.transcript'))
    screenshot('tasks-goal-armed.png')
    button('Pause after current round')
    until(lambda: has('Durable phase: Paused') and has('Current driving: Disarmed'))
    screenshot('tasks-goal-paused.png')
    provider.release('Native Goal hold')
    until(lambda: has('Driver: Disarmed') and text('.pane-status') == 'Completed')
    button('Resume Goal')
    until(lambda: has('Allocated rounds: 2 / 3') and has('Driver: Waiting'))
    until(lambda: text('.transcript').count('Waiting for native fixture release.') == 2)
    screenshot('tasks-goal-resumed.png')
    button('Cancel automatic round')
    until(lambda: text('.pane-status') == 'Cancelled' and has('Driver: Disarmed'))
    provider.release('Native Goal hold')
    screenshot('tasks-goal-cancelled.png')
    goal_text = text('#detail')
    allocated, cap = map(int, re.search(r'Allocated rounds: (\d+) / (\d+)', goal_text).groups())
    assert (allocated, cap) == (2, 3)
    close()
    send('Native background job', 'Started background Bash job')
    button('Current-Turn Jobs')
    until(lambda: has('Running') and has('Reported: false'))
    screenshot('tasks-jobs-running.png')
    (workspace / 'job-release').write_text('release\n')
    def finished():
        button('Refresh current Turn')
        return has('Completed') and has('Reported: false · Output retained: true')
    until(finished)
    screenshot('tasks-jobs-completed.png')
    provider.release('Native background job')
    until(lambda: text('.pane-status') == 'Failed')
    assert 'background jobs completed without an explicit report: job-1' in text('.transcript')
    jobs_terminal_text = text('.transcript')
    button('Refresh current Turn')
    until(lambda: has('No active Turn.'))
    screenshot('tasks-jobs-revoked.png')
    close()
    (report / 'tasks.json').write_text(json.dumps({'ok': True, 'native_clicks': True,
        'recorded_patch': True, 'inline_geometry': inline_geometry, 'goal_allocations_after_resume': allocated, 'goal_cap': cap,
        'goal_after_cancel_text': goal_text, 'jobs_terminal_text': jobs_terminal_text,
        'jobs_terminal': 'failed_unreported_job'}, indent=2))
