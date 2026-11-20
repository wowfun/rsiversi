import assert from 'node:assert/strict';
import {mkdir,writeFile} from 'node:fs/promises';
import {dirname,join,resolve} from 'node:path';
import {DatabaseSync} from 'node:sqlite';
import {chromium} from 'playwright';
import {chooseStartingWorkspace,connectWorkbench,openBrowserPage} from './browser-fixture.mjs';
import {cleanupAll} from './cleanup.mjs';
import {startService,waitUntil} from './service.mjs';

const report=resolve(process.env.RSI_REPORT_DIR);
await mkdir(report,{recursive:true});
const service=await startService({
  binary:resolve(process.env.RSI_WEB_BINARY),assets:resolve(process.env.RSI_WEB_ASSETS),report,
  foregroundHost:true,developmentHttp:process.platform==='win32',
});
const marker='Please hold this turn for the restart fixture.';
const automaticCalls=()=>service.provider.requests.filter(call=>call.prompt.includes(marker)).length;
const claims=facts=>facts.filter(fact=>fact.type==='input_message_claimed' && fact.source?.type==='continuation');
const errors=[];
let browser;
try {
  const opened=await openBrowserPage(chromium,errors);
  browser=opened.browser;
  const page=opened.page;
  await connectWorkbench(page,service,'Interrupted reminder');
  await chooseStartingWorkspace(page,service);
  const pane=page.getByRole('region',{name:'Main conversation',exact:true});
  await pane.getByRole('textbox',{name:'Main message'}).fill('Schedule the interrupted delivery fixture.');
  await pane.getByTestId('composer-send').click();
  await pane.getByRole('button',{name:'Open automation task: '+marker}).waitFor();
  const session=await pane.getAttribute('data-session-id');
  await browser.close();
  browser=undefined;

  await waitUntil(()=>automaticCalls()===1,'the automatic model request reaching the held provider');
  const before=await service.readSessionFacts(session);
  await writeFile(join(report,'before.json'),JSON.stringify(before,null,2));
  const accepted=claims(before);
  assert.equal(accepted.length,1);
  const {turn_id:turn,source:{message_id:message}}=accepted[0];
  const reminder=before.find(fact=>fact.type==='tool_result' && fact.result?.value?.status==='created_armed').result.value.state.reminders[0].id;
  assert(message && accepted[0].source.source.domain.id==='rsi.schedule');
  assert(before.some(fact=>fact.type==='model_started' && fact.turn_id===turn));
  assert(!before.some(fact=>fact.type==='turn_terminal' && fact.turn_id===turn));
  const callsBefore=service.provider.requests.length;
  const oldPid=service.foregroundPids[0];
  await service.restart(undefined,{crashHost:true});
  assert.notEqual(service.foregroundPids[0],oldPid);
  let facts,terminal;
  await waitUntil(async()=>{
    facts=await service.readSessionFacts(session);
    terminal=facts.find(fact=>fact.type==='turn_terminal' && fact.turn_id===turn);
    return terminal;
  },'original automatic turn reaching a terminal outcome after process termination',45_000);
  await writeFile(join(report,'after.json'),JSON.stringify(facts,null,2));
  assert.equal(claims(facts).length,1);
  assert.equal(claims(facts)[0].source.message_id,message);
  assert.equal(terminal.outcome.status,'interrupted');
  assert.equal(terminal.outcome.effect,'model');
  const db=new DatabaseSync(join(dirname(service.workspace),'state/rsiversi/agent/sessions.sqlite3'),{readOnly:true});
  let settled;
  try {
    const lookup=db.prepare(`SELECT c.control_json FROM domain_requests r JOIN agent_controls c
      ON c.session_id=r.session_id AND c.seq=r.control_seq WHERE r.session_id=? AND r.request_id='schedule-settle-1'`);
    await waitUntil(()=>{settled=lookup.get(session);return settled;},'reminder settlement on the replacement Host');
  } finally {db.close();}
  const update=JSON.parse(settled.control_json).commit.updates.find(value=>value.snapshot.identity.id==='rsi.schedule');
  const state=update.snapshot.state;
  assert.equal(state.allocated_rounds,1);
  assert.equal(state.reminders.length,1);
  assert.equal(state.reminders[0].id,reminder);
  assert.equal(state.reminders[0].consumed,true);
  assert.equal(state.reminders[0].active,false);
  assert.equal(state.reservation.input.message_id,message);
  assert.equal(state.reservation.settled,true);
  assert.equal(automaticCalls(),1);
  assert.equal(service.provider.requests.length,callsBefore);
  assert.deepEqual(errors,[]);
  await writeFile(join(report,'result.json'),JSON.stringify({
    platform:process.platform,oldPid,newPid:service.foregroundPids[0],turn,message,
    providerCalls:callsBefore,automaticCalls:automaticCalls(),outcome:terminal.outcome,settledState:state,
  },null,2));
} finally {
  await cleanupAll(()=>browser?.close(),()=>service.close());
}
