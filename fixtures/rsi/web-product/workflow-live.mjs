import {openBrowserPage, connectWorkbench, openWorkspace} from './browser-fixture.mjs';
import {cleanupAll} from './cleanup.mjs';
// Explicit live native workflow proof through the actual browser and Host.
import assert from 'node:assert/strict';
import {execFileSync} from 'node:child_process';
import {writeFile,mkdir,cp,readFile} from 'node:fs/promises';
import {join} from 'node:path';
import {chromium} from 'playwright';
import {liveFixture, configureProgram} from './live-fixture.mjs';
import {assertNoNotices} from './task-checks.mjs';
import {openResource,details} from './controls.mjs';
import {assertNoPendingWorkflowCleanup,nativePrograms,programEvidence} from './program-fixture.mjs';
import {paired} from './paired-env.mjs';

const mode = process.env.RSI_WORKFLOW_MODE ?? 'background';
assert(['background', 'foreground', 'cancel', 'plan'].includes(mode));
const cancellation = mode === 'cancel' || mode === 'plan';
const skill=process.env.RSI_WORKFLOW_SKILL;
assert(!skill || ['workflow-collect','workflow-review'].includes(skill));
const reviewing=skill==='workflow-review';

const fixture = await liveFixture({configure: async ({config, workspace}) => {
  await configureProgram({config});
  if(skill) {
    const destination=join(workspace,'.agents/skills',skill);
    await mkdir(destination,{recursive:true});
    await cp(`examples/rsi/workflow/skills/${skill}/SKILL.md`,join(destination,'SKILL.md'));
    const settings=JSON.parse(await readFile(join(config,'settings.json'),'utf8'));
    settings['rsi.agent'].sandbox='read-only';
    await writeFile(join(config,'settings.json'),JSON.stringify(settings));
  }
  await writeFile(join(workspace, 'left.json'), JSON.stringify({n: 19}));
  await writeFile(join(workspace, 'right.json'), JSON.stringify({n: 23}));
  await writeFile(join(workspace, 'review.txt'), 'Arithmetic example: 19 + 23 = 41.\n');
}});
const {service, report, model} = fixture;
const errors = [];
const requestsFailed=[];
let page, browser;

try {
  ({browser, page} = await openBrowserPage(chromium, errors));
  page.on('requestfailed',request=>{if(requestsFailed.length<32)requestsFailed.push({method:request.method(),path:new URL(request.url()).pathname,error:request.failure()?.errorText});});
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
  const finalMarker = cancellation ? 'LIVE_WORKFLOW_CANCELLED' : reviewing?'LIVE_WORKFLOW_REVIEW_VERIFIED':'LIVE_WORKFLOW_TOTAL_42';
  const afterCompletion = cancellation ? `verify that its actual outcome is cancelled and answer ${finalMarker}` : `verify the actual total and answer ${finalMarker} only when the actual total is 42`;
  await input.fill(skill ? `Discover the available Skills, then call skill_read with name "${skill}". Execute its exact ${reviewing?'serial review and independent verification':'parallel collection and serial verification'} tutorial once using run_workflow with background=true. End the creator Turn with WORKFLOW_DETACHED. When its actual completion notice arrives, call workflow_read to confirm ${reviewing?'verification.verified=true and the source arithmetic error':'total=42 and verified=true'}. The completion notice alone does not satisfy this required tool read. Only after a successful workflow_read of the completed result, answer ${finalMarker}. Do not create another run or modify files.` : `Isolated integration verification. Call run_workflow exactly once with ${observation} and this exact script: ${script}\nAfter the tool returns running, end this Turn with WORKFLOW_DETACHED. Do not wait or poll. When a later workflow completion notice arrives, you must call workflow_read for that exact run. The notice alone does not satisfy this required tool read. Only after a successful workflow_read of its terminal state, ${afterCompletion}. Do not create another workflow or modify files.`);
  const started = Date.now(); await pane.getByTestId('composer-send').click();
  await page.waitForFunction(() => document.querySelector('[aria-label="Main message"]')?.value === '');
  let processIdentity;
  if (cancellation) {
    await pane.locator('.pane-status').getByText('Completed', {exact:true}).waitFor();
    const pending = programEvidence(service.workspace);
    const accepted = pending.controls.find(({record})=>record.type==='program_run' && record.event.event==='accepted'); assert(accepted);
    const progress = pending.controls.find(({record})=>record.type==='program_run' && record.event.event==='progress'); assert(progress);
    const nodes=await nativePrograms(service,JSON.parse(progress.record.event.message).pid);await writeFile(join(report,'process-candidates.json'),JSON.stringify(await nativePrograms(service,undefined,true),null,2));assert.equal(nodes.length,1,'one actual host Node in this isolated workspace');
    processIdentity={...nodes[0],namespace_pid:JSON.parse(progress.record.event.message).pid};
    await writeFile(join(report,'process.json'),JSON.stringify(processIdentity));
    if(mode==='plan') {
      await input.fill('/plan on');
      await pane.getByTestId('composer-send').click();
      await page.waitForFunction(() => document.querySelector('[aria-label="Main message"]')?.value === '');
    } else {
      await openResource(page,'Workflows');
      await details(page).getByRole('button',{name:'Open workflow',exact:true}).first().click();
      await details(page).getByRole('button',{name:'Cancel workflow',exact:true}).click();
      await page.waitForFunction(()=>[...document.querySelectorAll('.resource-content')].some(e=>e.textContent.includes('State: Cancelled')));
      assertNoPendingWorkflowCleanup(await details(page).innerText());
      await page.screenshot({path:join(report,'user-cancel.png')});
    }
  }
  const deadline = Date.now() + 240000;
  let data;
  while (Date.now() < deadline) {
    data = programEvidence(service.workspace);
    const terminal = data.controls.find(({record}) => record.type === 'program_run' && record.event.event === 'terminal');
    const assistantText = data.facts.filter(({record}) => record.type === 'model_event').map(({record}) => record.event?.type === 'content_delta' && record.event.delta?.type === 'text' ? record.event.delta.value : '').join('');
    if (terminal && assistantText.includes(finalMarker) && await pane.locator('.pane-status').innerText() === 'Completed') break;
    if (terminal && !['completed', ...(cancellation?['cancelled']:[])].includes(terminal.record.event.outcome.status)) break;
    await page.waitForTimeout(200);
  }
  data = programEvidence(service.workspace); await writeFile(join(report, 'evidence.json'), JSON.stringify(data, null, 2));
  const transcript = await pane.locator('.transcript').innerText(); await writeFile(join(report, 'transcript.txt'), transcript);
  const runs = data.controls.filter(({record}) => record.type === 'program_run');
  const accepted = runs.filter(({record}) => record.event.event === 'accepted'); assert.equal(accepted.length, 1);
  const root = accepted[0].session_id, descriptor = accepted[0].record.event.descriptor;
  const terminal = runs.find(({record}) => record.event.event === 'terminal'); assert.equal(terminal?.record.event.outcome.status, cancellation?'cancelled':'completed', transcript.slice(-4000));
  assert(runs.some(({record}) => record.event.event === 'detached'));
  const parentEnd = data.controls.find(({session_id, record}) => session_id === root && record.type === 'activation_settled'); assert(parentEnd);
  const admissions = runs.filter(({record}) => record.event.event === 'child_admitted'); assert.equal(admissions.length, cancellation?0:2);
  if(!skill)assert(admissions.every(({record}) => record.seq > parentEnd.record.seq), 'delayed children are admitted after creator activation settlement');
  else assert(terminal.record.seq>parentEnd.record.seq,'tutorial survives creator activation settlement');
  const receipts = runs.filter(({record}) => record.event.event === 'child_settled').map(({record}) => record.event.receipt);
  assert.equal(receipts.length, cancellation?0:2); assert(receipts.every(receipt => receipt.outcome.status === 'completed' && receipt.result));
  if(cancellation) {
    assert.deepEqual(await nativePrograms(service),[],'no Node remains in the isolated workspace');
    const stillSame=execFileSync('python3',['-c','import pathlib,sys;p=pathlib.Path("/proc/"+sys.argv[1]+"/stat");print(p.exists() and p.read_text().rsplit(")",1)[1].split()[19]==sys.argv[2])',String(processIdentity.pid),processIdentity.start_time],{encoding:'utf8'}).trim();
    assert.equal(stillSame,'False','the exact host Node process was reaped');
  }
  const childMessages = data.controls.filter(({session_id, record}) => session_id === root && record.type === 'message_accepted' && record.message.source.type === 'completion'); assert.equal(childMessages.length, 0);
  const notices = data.controls.filter(({session_id, record}) => session_id === root && record.type === 'message_accepted' && record.message.source.type === 'program'); assert.equal(notices.length, 1);
  const toolIntents = data.facts.filter(({record}) => record.type === 'tool_intent');
  const invocations=toolIntents.filter(({record}) => record.name === 'run_workflow');
  assert.equal(invocations.length, 1);
  const invocation=invocations[0];assert.equal(invocation.session_id,root);
  if(!skill) {
    assert.equal(invocation.record.arguments.script,script,'actual model-submitted script matches the requested fixture');
    assert.equal(invocation.record.arguments.background,mode!=='foreground');
    if(mode==='foreground')assert.equal(invocation.record.arguments.observe_seconds,1);
  }
  const returned=data.facts.find(({session_id,record})=>session_id===root && record.type==='tool_result' && record.effect_id===invocation.record.effect_id);
  assert(returned && !returned.record.result.is_error);
  assert.equal(returned.record.result.value.run.run_id,descriptor.run_id);
  assert.equal(returned.record.result.value.status,'running');
  assert.equal(returned.record.result.value.detached,true);
  const readEffects=new Set(toolIntents.filter(({session_id,record})=>session_id===root && record.name==='workflow_read' && record.arguments.run_id===descriptor.run_id).map(({record})=>record.effect_id));
  assert(readEffects.size>0,'model explicitly reads the exact Workflow');
  const snapshots=data.facts.filter(({session_id,record})=>session_id===root && record.type==='tool_result' && readEffects.has(record.effect_id) && !record.result?.is_error).map(({record})=>record.result?.value?.fragment).filter(value=>typeof value==='string').map(value=>{try{return JSON.parse(value)}catch{return null}}).filter(value=>value?.run_id===descriptor.run_id && value.outcome?.status===(cancellation?'cancelled':'completed'));
  assert(snapshots.length>0,'model read the actual terminal Workflow snapshot');
  const actualSnapshot=snapshots.at(-1);
  assert.equal(actualSnapshot.control_seq,terminal.record.seq);
  const actual=actualSnapshot.result;
  if(!skill && !cancellation) {
    assert.equal(actual.total,42);
    assert.equal(actual.receipts.length,2);
    assert.equal(actual.references.length,2);
  }
  if(skill) {
    assert(toolIntents.some(({record}) => record.name === 'skill_read'),'real model read the discovered Skill body');
    assert(data.facts.some(({record})=>record.type==='tool_result' && record.result?.value?.resource?.id===skill && !record.result.is_error),'Skill body read succeeded');
    assert.equal(descriptor.sandbox,'read-only','actual Workflow captures enforced read-only sandbox');
    if(reviewing) {
      assert.equal(actual.verification.verified,true);
      assert(actual.finding.evidence.includes('19 + 23 = 41'));
      const first=runs.find(({record})=>record.event.event==='child_settled'&&record.event.receipt.ordinal===1);
      const second=runs.find(({record})=>record.event.event==='child_admitted'&&record.event.ordinal===2);
      assert(second.record.seq>first.record.seq,'verifier admitted only after the review settled');
    } else assert.deepEqual(actual,{total:42,verified:true});
  }
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
  await writeFile(join(report, 'result.json'), JSON.stringify({ok: true, family:paired.family, mode, skill:skill??null, browser: browser.version(), model, reasoning_effort: 'off', elapsed_ms: Date.now() - started, run_id: descriptor.run_id, mock_model_requests: 0, model_requests: data.facts.filter(({record}) => record.type === 'model_intent').length, receipts, scope: cancellation ? 'One live workflow cancellation with zero child admissions and verified host Node reaping.' : reviewing?'One live serial review and independent source verification with structured receipts.':skill?'One live detached tutorial with parallel structured children and serial source verification; terminal completion follows creator settlement.':'One live detached workflow with two parallel structured children after creator settlement.'}, null, 2));
} catch (error) {
  await writeFile(join(report,'failure-evidence.json'),JSON.stringify(programEvidence(service.workspace),null,2)).catch(()=>{});
  await writeFile(join(report,'browser-failure.json'),JSON.stringify({family:paired.family,errors,requestsFailed},null,2)).catch(()=>{});
  if (page) await page.screenshot({path: join(report, 'failure.png')}).catch(() => {});
  await writeFile(join(report, 'failure.txt'), fixture.redact(error)); throw new Error(fixture.redact(error));
} finally { await cleanupAll(() => browser?.close(), () => fixture.close()); }
