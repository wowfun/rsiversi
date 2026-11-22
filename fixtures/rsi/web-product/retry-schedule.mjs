import assert from 'node:assert/strict';
import { once } from 'node:events';
import { mkdir, writeFile } from 'node:fs/promises';
import { createServer } from 'node:http';
import { join, resolve } from 'node:path';
import { chromium } from 'playwright';
import { startService, waitUntil } from './service.mjs';
import { deepseekProfile } from './live-limits.mjs';
import {attemptEvents} from './model-attempt.mjs';
import {chooseStartingWorkspace, connectHostOwner} from './browser-fixture.mjs';
import {navigationMode, openConversation} from './controls.mjs';
import {setTimeout as sleep} from 'node:timers/promises';

const { RSI_WEB_BINARY: binary, RSI_WEB_ASSETS: assets, RSI_WEB_REPORT: output } = process.env;
assert(binary && assets && output);
const report = resolve(output);
await mkdir(report, {recursive: true});
let calls = 0;
const receivedAt=[];
const cancel = process.argv.includes('--cancel-reopen');
const exhaust = process.argv.includes('--exhaust');
const delay = cancel ? 9 : exhaust ? 3 : 6;
const provider = createServer(async (request, response) => {
  for await (const _ of request) { /* consume the real request */ }
  calls++;
  receivedAt.push(Date.now());
  response.writeHead(calls === 1 || exhaust ? 429 : 400, {'content-type':'application/json', 'retry-after':String(delay)});
  response.end(JSON.stringify({error:{message:'controlled provider refusal'}}));
}).listen(0, '127.0.0.1');
await once(provider, 'listening');
let service, browser, page;
try {
  const model = 'fixture-model';
  service = await startService({binary, assets, report, localWeb:true, developmentHttp:true,
    foregroundHost:process.platform === 'win32', deepseekKey:'isolated-loopback-fixture',
    configure: async ({config}) => {
      const profile = deepseekProfile(model, {context_window_tokens:128000,default_output_reserve_tokens:4096,max_output_reserve_tokens:8192})
        .replace('https://api.deepseek.com', 'http://127.0.0.1:' + provider.address().port);
      await writeFile(join(config,'host-profiles/fixture/host.profile.toml'), profile);
      await writeFile(join(config,'settings.json'),JSON.stringify({'rsi.agent':{default_model:{deployment:'live',model}}}));
    }});
  browser = await chromium.launch();
  page = await browser.newPage({viewport:{width:1440,height:960},colorScheme:'dark'});
  await connectHostOwner(page,service);
  await chooseStartingWorkspace(page,service);
  const composer = page.getByRole('textbox',{name:'Main message',exact:true});
  const send = async text => { await composer.fill(text); await page.getByTestId('composer-send').click(); };
  await send('/compact');
  const transcript=page.getByRole('region',{name:'Conversation transcript'});
  await transcript.getByRole('button',{name:/No eligible history to compact/}).waitFor();
  assert.equal(await page.getByText('No eligible history to compact.',{exact:true}).count(),1);
  assert.equal(calls,0);
  await page.screenshot({path:join(report,'compact-empty.png')});
  await composer.fill('Exercise the retry schedule.');
  await page.getByTestId('composer-send').click();
  const pane = page.getByRole('region',{name:'Main conversation',exact:true});
  await waitUntil(()=>calls>=1,'first model request reaching the provider');
  const session = await pane.getAttribute('data-session-id');
  let decision, limit;
  await waitUntil(async()=>{
    const facts=await service.readSessionFacts(session);
    decision=facts.find(f=>f.attempt?.scheduled_retry);
    if (decision) limit=facts.find(f=>f.type==='model_intent' && f.effect_id===decision.effect_id).snapshot.retry_policy.max_retries;
    return decision;
  },'first durable retry decision',5000);
  assert.equal(decision.attempt.scheduled_retry.delay_ms,delay*1000);
  await writeFile(join(report,'decision.json'),JSON.stringify({seq:decision.seq,turn:decision.turn_id,retry:decision.attempt.scheduled_retry}));
  const row = transcript.locator('details').filter({has:page.getByText(/Retrying model request/)});
  await row.locator('summary').click({timeout:8000});
  assert.match(await row.innerText(),/provider\.rate_limited/);
  assert.match(await row.innerText(),new RegExp(delay*1000+' ms'));
  const firstLabel=await row.locator('summary').innerText();
  await waitUntil(async()=>await row.locator('summary').innerText()!==firstLabel,'active countdown changing',3000);
  await page.screenshot({path:join(report,'retry-scheduled.png')});
  if (cancel) {
    await pane.getByRole('button',{name:'Stop',exact:true}).click();
    await transcript.getByText(/Model request retry cancelled/).waitFor();
    await page.screenshot({path:join(report,'retry-cancelled.png')});
  } else {
    await send('/compact');
    await transcript.getByRole('button',{name:/Session is busy\. Run \/compact/}).waitFor();
    await page.screenshot({path:join(report,'compact-busy.png')});
    if (exhaust) {
      for(let i=2;i<=limit;i++) {
        const next=transcript.locator('details').filter({has:page.getByText(new RegExp('Retrying model request \\('+i+'/'+limit+'\\)'))});
        await next.waitFor();
        assert.equal(await next.getAttribute('open'),'');
        assert.equal(await transcript.locator('details').filter({hasText:/Retrying model request|Retried model request/}).count(),1);
      }
    }
    await transcript.getByText(/Retried model request/).waitFor();
    await page.screenshot({path:join(report,'retry-started.png')});
  }
  let facts;
  await waitUntil(async () => {
    facts = await service.readSessionFacts(session);
    return facts.some(f=>f.type==='turn_terminal');
  },'turn settled',30000);
  const scheduled=facts.find(f=>f.type==='model_attempt_settled' && f.attempt.scheduled_retry);
  assert(scheduled);
  assert.equal(Array.from(attemptEvents(scheduled.attempt)).at(-1).event.type,'failed');
  const retry=scheduled.attempt.scheduled_retry;
  assert.equal(retry.delay_ms,delay*1000);
  const terminal = facts.find(f=>f.type==='turn_terminal' && f.turn_id===scheduled.turn_id);
  assert.equal(terminal.outcome.status,cancel?'cancelled':'failed');
  if (cancel) {
    await page.goto('about:blank');
    await service.restart(undefined,{crashHost:true});
    await page.goto(service.origin);
    await page.getByLabel('Allow local HTTP for development',{exact:true}).check();
    await page.getByRole('button',{name:'Reconnect with this browser',exact:true}).click();
    await page.locator('#workbench').waitFor({state:'visible'});
    await navigationMode(page,'flat');
    if (await pane.getAttribute('data-session-id') !== session) await openConversation(page,pane,session);
    await transcript.getByText(/Model request retry cancelled/).waitFor();
    await page.setViewportSize({width:420,height:830});
    await page.screenshot({path:join(report,'retry-reopened.png')});
    await sleep(Math.max(0,scheduled.timestamp_ms+retry.delay_ms+500-Date.now()));
    facts=await service.readSessionFacts(session);
    assert.equal(facts.filter(f=>f.type==='turn_terminal' && f.turn_id===terminal.turn_id).length,1);
    assert.deepEqual(facts.find(f=>f.seq===terminal.seq),terminal);
  }
  assert.equal(calls,cancel?1:exhaust?limit+1:2);
  assert(!facts.some(f=>f.type==="manual_compaction"));
  const started=facts.filter(f=>f.type==='model_started' && f.effect_id===retry.effect_id && f.seq>scheduled.seq);
  assert.equal(started.length,cancel?0:1);
  const decisions=facts.filter(f=>f.attempt?.scheduled_retry);
  assert.deepEqual(decisions.map(f=>f.attempt.scheduled_retry.number),Array.from({length:exhaust?limit:1},(_,i)=>i+1));
  if (!cancel) for (const [i, fact] of decisions.entries()) {
    const earliest=fact.timestamp_ms+fact.attempt.scheduled_retry.delay_ms;
    assert(receivedAt[i+1]>=earliest,'provider received a reserved retry before its selected wait ended');
    assert(facts.some(f=>f.type==='model_started' && f.effect_id===fact.attempt.scheduled_retry.effect_id && f.seq>fact.seq));
  }
  if (exhaust) {
    assert.equal(terminal.outcome.code,'provider.rate_limited');
  }
  await writeFile(join(report,'result.json'),JSON.stringify({ok:true,browser:browser.version(),requests:calls,retry,
    scheduled_sequence:scheduled.seq,scheduled_at:scheduled.timestamp_ms,limit,decisions:decisions.map(f=>({seq:f.seq,...f.attempt.scheduled_retry})),receivedAt,started,terminal,checked_at:Date.now()},null,2));
} catch(error) {
  await page?.screenshot({path:join(report,'failure.png')}).catch(()=>{});
  await writeFile(join(report,'result.json'),JSON.stringify({ok:false,requests:calls,error:error.message},null,2));
  throw error;
} finally {
  await browser?.close();
  await service?.close();
  provider.closeAllConnections();
  await new Promise(resolve=>provider.close(resolve));
}
