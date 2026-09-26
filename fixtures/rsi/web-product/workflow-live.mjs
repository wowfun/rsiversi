import {openBrowserPage, connectWorkbench, openWorkspace} from './browser-fixture.mjs';
import {cleanupAll} from './cleanup.mjs';
// Explicit live native workflow proof through the actual browser and Host.
import assert from 'node:assert/strict';
import {execFileSync} from 'node:child_process';
import {writeFile} from 'node:fs/promises';
import {join} from 'node:path';
import {chromium} from 'playwright';
import {liveFixture, configureProgram} from './live-fixture.mjs';
import {assertNoNotices} from './task-checks.mjs';

const mode = process.env.RSI_WORKFLOW_MODE ?? 'background';
assert(['background', 'foreground', 'cancel', 'plan'].includes(mode));
const cancellation = mode === 'cancel' || mode === 'plan';

const fixture = await liveFixture({configure: async ({config, workspace}) => {
  await configureProgram({config});
  await writeFile(join(workspace, 'left.json'), JSON.stringify({n: 19}));
  await writeFile(join(workspace, 'right.json'), JSON.stringify({n: 23}));
}});
const {service, report, model} = fixture;
const errors = [];
let page, browser;
function evidence() {
  const script = `import json,sqlite3,pathlib,sys
paths=list((pathlib.Path(sys.argv[1]).parent/'state').rglob('sessions.sqlite3'))
assert len(paths)==1,paths
db=sqlite3.connect('file:'+str(paths[0])+'?mode=ro',uri=True)
print(json.dumps({kind:[{'session_id':s,'record':json.loads(v)} for s,v in db.execute(sql)] for kind,sql in [('controls','SELECT session_id,control_json FROM agent_controls ORDER BY session_id,seq'),('facts','SELECT session_id,fact_json FROM facts ORDER BY session_id,seq')]}))`;
  return JSON.parse(execFileSync('python3', ['-c', script, service.workspace], {encoding: 'utf8', maxBuffer: 16 * 1024 * 1024}));
}
function nativeNodes() {
  const script=`import json,pathlib,sys
found=[]
for path in pathlib.Path('/proc').iterdir():
 if not path.name.isdigit():continue
 try:
  if (path/'comm').read_text().strip()!='node' or str((path/'cwd').resolve())!=sys.argv[1]:continue
  stat=(path/'stat').read_text().rsplit(')',1)[1].split()
  found.append({'pid':int(path.name),'start_time':stat[19]})
 except (OSError,PermissionError):pass
print(json.dumps(found))`;
  return JSON.parse(execFileSync('python3',['-c',script,service.workspace],{encoding:'utf8'}));
}
try {
  ({browser, page} = await openBrowserPage(chromium, errors));
  await connectWorkbench(page, service, 'live background workflow');
  await openWorkspace(page, service);
  const pane = page.getByRole('region', {name: 'Main conversation', exact: true});
  const input = pane.getByRole('textbox', {name: 'Main message', exact: true});
  const schema = {type: 'object', properties: {n: {type: 'integer'}}, required: ['n'], additionalProperties: false};
  const task = file => ({message: `Read ${file} using file_read, then call report_result with the actual n as {"n":number}. Do not spawn agents or modify files.`, output_schema: schema});
  const script = cancellation
    ? `await workflow.phase('Waiting for cancellation',{pid:process.pid}); await new Promise(()=>{}); return {unexpected:true};`
    : `await workflow.phase('Collect evidence',{jobs:2}); await new Promise(resolve=>setTimeout(resolve,8000)); const rows=await workflow.parallel([()=>workflow.agent(${JSON.stringify(task('left.json'))}),()=>workflow.agent(${JSON.stringify(task('right.json'))})]); await workflow.log({received:rows.length}); return await workflow.pipeline([async rows=>({total:rows.reduce((sum,row)=>sum+row.value.n,0),receipts:rows.map(row=>row.receipt),references:rows.map(row=>row.reference)})],rows);`;
  const observation = mode === 'foreground' ? 'background=false and observe_seconds=1' : 'background=true';
  const finalMarker = cancellation ? 'LIVE_WORKFLOW_CANCELLED' : 'LIVE_WORKFLOW_TOTAL_42';
  const afterCompletion = cancellation ? `verify that its actual outcome is cancelled and answer ${finalMarker}` : `verify the actual total and answer ${finalMarker} only when the actual total is 42`;
  await input.fill(`Isolated integration verification. Call run_workflow exactly once with ${observation} and this exact script: ${script}\nAfter the tool returns running, end this Turn with WORKFLOW_DETACHED. Do not wait or poll. When a later workflow completion notice arrives, call workflow_read for that run, ${afterCompletion}. Do not create another workflow or modify files.`);
  const started = Date.now(); await pane.getByRole('button', {name: 'Send ↗', exact: true}).click();
  await page.waitForFunction(() => document.querySelector('[aria-label="Main message"]')?.value === '');
  let processIdentity;
  if (cancellation) {
    await pane.locator('.pane-status').getByText('Completed', {exact:true}).waitFor();
    const pending = evidence();
    const accepted = pending.controls.find(({record})=>record.type==='program_run' && record.event.event==='accepted'); assert(accepted);
    const progress = pending.controls.find(({record})=>record.type==='program_run' && record.event.event==='progress'); assert(progress);
    const nodes=nativeNodes();assert.equal(nodes.length,1,'one actual host Node in this isolated workspace');
    processIdentity={...nodes[0],namespace_pid:JSON.parse(progress.record.event.message).pid};
    await writeFile(join(report,'process.json'),JSON.stringify(processIdentity));
    await input.fill(mode === 'plan' ? '/plan on' : `Call workflow_cancel for run_id ${accepted.record.run_id}, then read its actual outcome using workflow_read. Finish with LIVE_WORKFLOW_CANCELLED only after confirming cancelled. Do not create another run.`);
    await pane.getByRole('button',{name:'Send ↗',exact:true}).click();
    await page.waitForFunction(() => document.querySelector('[aria-label="Main message"]')?.value === '');
  }
  const deadline = Date.now() + 240000;
  let data;
  while (Date.now() < deadline) {
    data = evidence();
    const terminal = data.controls.find(({record}) => record.type === 'program_run' && record.event.event === 'terminal');
    const assistantText = data.facts.filter(({record}) => record.type === 'model_event').map(({record}) => record.event?.type === 'content_delta' && record.event.delta?.type === 'text' ? record.event.delta.value : '').join('');
    if (terminal && assistantText.includes(finalMarker) && await pane.locator('.pane-status').innerText() === 'Completed') break;
    if (terminal && !['completed', ...(cancellation?['cancelled']:[])].includes(terminal.record.event.outcome.status)) break;
    await page.waitForTimeout(200);
  }
  data = evidence(); await writeFile(join(report, 'evidence.json'), JSON.stringify(data, null, 2));
  const transcript = await pane.locator('.transcript').innerText(); await writeFile(join(report, 'transcript.txt'), transcript);
  const runs = data.controls.filter(({record}) => record.type === 'program_run');
  const accepted = runs.filter(({record}) => record.event.event === 'accepted'); assert.equal(accepted.length, 1);
  const root = accepted[0].session_id, descriptor = accepted[0].record.event.descriptor;
  const terminal = runs.find(({record}) => record.event.event === 'terminal'); assert.equal(terminal?.record.event.outcome.status, cancellation?'cancelled':'completed', transcript.slice(-4000));
  assert(runs.some(({record}) => record.event.event === 'detached'));
  const parentEnd = data.controls.find(({session_id, record}) => session_id === root && record.type === 'activation_settled'); assert(parentEnd);
  const admissions = runs.filter(({record}) => record.event.event === 'child_admitted'); assert.equal(admissions.length, cancellation?0:2);
  assert(admissions.every(({record}) => record.seq > parentEnd.record.seq), 'both children are admitted after creator activation settlement');
  const receipts = runs.filter(({record}) => record.event.event === 'child_settled').map(({record}) => record.event.receipt);
  assert.equal(receipts.length, cancellation?0:2); assert(receipts.every(receipt => receipt.outcome.status === 'completed' && receipt.result));
  if(cancellation) {
    assert.deepEqual(nativeNodes(),[],'no Node remains in the isolated workspace');
    const stillSame=execFileSync('python3',['-c','import pathlib,sys;p=pathlib.Path("/proc/"+sys.argv[1]+"/stat");print(p.exists() and p.read_text().rsplit(")",1)[1].split()[19]==sys.argv[2])',String(processIdentity.pid),processIdentity.start_time],{encoding:'utf8'}).trim();
    assert.equal(stillSame,'False','the exact host Node process was reaped');
  }
  const childMessages = data.controls.filter(({session_id, record}) => session_id === root && record.type === 'message_accepted' && record.message.source.type === 'completion'); assert.equal(childMessages.length, 0);
  const notices = data.controls.filter(({session_id, record}) => session_id === root && record.type === 'message_accepted' && record.message.source.type === 'program'); assert.equal(notices.length, 1);
  const toolIntents = data.facts.filter(({record}) => record.type === 'tool_intent');
  assert.equal(toolIntents.filter(({record}) => record.name === 'run_workflow').length, 1);
  assert(toolIntents.some(({record}) => record.name === 'workflow_read'));
  const replies = data.facts.filter(({session_id, record}) => session_id === root && record.type === 'model_event').map(({record}) => record.event?.type === 'content_delta' && record.event.delta?.type === 'text' ? record.event.delta.value : '').join('');
  assert(replies.includes(finalMarker)); assert.match(replies, /WORKFLOW_DETACHED/);
  await page.getByRole('region', {name: 'Needs attention', exact: true}).getByText('Running', {exact: true}).waitFor({state: 'hidden'});
  await page.screenshot({path: join(report, 'workflow-wide.png')});
  await pane.locator('.message-title').filter({hasText: 'run_workflow'}).first().scrollIntoViewIfNeeded();
  await page.screenshot({path: join(report, 'workflow-card-wide.png')});
  await page.setViewportSize({width: 420, height: 900});
  await pane.locator('.message-title').filter({hasText: 'workflow_read'}).first().scrollIntoViewIfNeeded();
  assert(await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth));
  await page.screenshot({path: join(report, 'workflow-narrow.png')});
  await assertNoNotices(page); assert.deepEqual(errors, []); assert.equal(service.provider.requests.length, 0);
  await writeFile(join(report, 'result.json'), JSON.stringify({ok: true, mode, browser: browser.version(), model, reasoning_effort: 'off', elapsed_ms: Date.now() - started, run_id: descriptor.run_id, mock_model_requests: 0, model_requests: data.facts.filter(({record}) => record.type === 'model_intent').length, receipts, scope: cancellation ? 'One live workflow cancellation with zero child admissions and verified host Node reaping.' : 'One live detached workflow with two parallel structured children after creator settlement.'}, null, 2));
} catch (error) {
  if (page) await page.screenshot({path: join(report, 'failure.png')}).catch(() => {});
  await writeFile(join(report, 'failure.txt'), fixture.redact(error)); throw new Error(fixture.redact(error));
} finally { await cleanupAll(() => browser?.close(), () => fixture.close()); }
