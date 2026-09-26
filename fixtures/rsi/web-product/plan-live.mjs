import {openBrowserPage, connectWorkbench, openWorkspace} from './browser-fixture.mjs';
import {cleanupAll} from './cleanup.mjs';
// Explicit live plan handoff and structured delegation; never a default test.
import assert from 'node:assert/strict';
import {writeFile} from 'node:fs/promises';
import {join} from 'node:path';
import {chromium} from 'playwright';
import {liveFixture} from './live-fixture.mjs';
import {assertNoNotices} from './task-checks.mjs';
const fixture=await liveFixture();
const {report,model,service}=fixture;
let page,browser;const errors=[],started=Date.now();let approvals=0;
try {
  ({browser, page} = await openBrowserPage(chromium, errors));
  await connectWorkbench(page, service, 'live plan and structured delegation');
  await openWorkspace(page, service);
  const pane=page.getByRole('region',{name:'Main conversation',exact:true}),input=pane.getByRole('textbox',{name:'Main message',exact:true});
  await input.fill('/plan on');await pane.getByRole('button',{name:'Send ↗',exact:true}).click();await pane.locator('.command-receipt').filter({hasText:'Draft changed'}).waitFor();
  await input.fill('This is an isolated integration test. First call plan_write with title "Structured delegation verification" and a body explaining only this harmless task: after human approval, delegate computation of 6 * 7 and read its validated result. Then call request_plan_execution with the exact plan_ref. After approval, use spawn_agent with task_name "calculation", fork_turns "none", message asking the child to return {"answer":42} using report_result, and output_schema {"type":"object","properties":{"answer":{"type":"integer"}},"required":["answer"],"additionalProperties":false}. Wait for that child to complete. Call read_agent_result using the exact result locator returned in its completion and default page size. Do not use bash or change any files. Finish with LIVE_PLAN_STRUCTURED_42 only if the returned fragment actually contains answer 42. Do all these steps through the named tools.');
  await pane.getByRole('button',{name:'Send ↗',exact:true}).click();
  const deadline=Date.now()+240000;
  while(Date.now()<deadline) {
    const answer=page.getByRole('region',{name:'Needs attention',exact:true}).getByRole('button',{name:'Answer question 1',exact:true});
    if(await answer.count() && await answer.isVisible()) {
      assert.equal(approvals,0,'unexpected additional review');await answer.click();const dialog=page.locator('#detail');await dialog.getByText('Review plan',{exact:true}).waitFor();
      const plan=await dialog.locator('.review-plan').innerText();assert.match(plan,/6\s*[*×]\s*7|42|comput|计算|delegat/i);await writeFile(join(report,'reviewed-plan.txt'),plan);await page.screenshot({path:join(report,'live-review.png')});
      await dialog.getByRole('button',{name:'Approve and execute',exact:true}).click();approvals++;await dialog.waitFor({state:'hidden'});await answer.waitFor({state:'hidden'});
    }
    const status=await pane.locator('.pane-status').innerText();if(['Completed','Failed'].includes(status))break;
    await page.waitForTimeout(100);
  }
  const transcript=await pane.locator('.transcript').innerText();await writeFile(join(report,'transcript.txt'),transcript);
  assert.equal(await pane.locator('.pane-status').innerText(),'Completed',transcript.slice(-4096));assert.equal(approvals,1);assert.match(transcript,/LIVE_PLAN_STRUCTURED_42/);
  const session=(await pane.locator('.pane-session').innerText()).split(' · ').at(-1);
  const records=service.run(['--profile','evidence-cli','--resume',session,'--output','jsonl'],{input:':history\n'.repeat(64)+':exit\n'}).stdout.trim().split('\n').map(line=>JSON.parse(line));
  const facts=records.flatMap(record=>record.fact?[record.fact]:[]).sort((a,b)=>a.seq-b.seq);await writeFile(join(report,'facts.json'),JSON.stringify(facts,null,2));
  const calls=name=>facts.filter(f=>f.type==='tool_intent' && f.name===name).map(intent=>({intent,result:facts.find(f=>f.type==='tool_result' && f.effect_id===intent.effect_id)}));
  for(const name of ['plan_write','request_plan_execution','spawn_agent','read_agent_result'])assert(calls(name).some(call=>call.result && !call.result.result.is_error),`successful ${name} evidence missing`);
  assert(calls('spawn_agent').some(call=>call.intent.arguments.output_schema?.properties?.answer?.type==='integer'));
  assert(calls('read_agent_result').some(call=>call.result?.result.value.complete && JSON.parse(call.result.result.value.fragment).answer===42));
  assert.equal(service.provider.requests.length,0);await page.screenshot({path:join(report,'live-completed.png')});await assertNoNotices(page);assert.deepEqual(errors,[]);
  await writeFile(join(report,'result.json'),JSON.stringify({ok:true,browser:browser.version(),model,reasoning_effort:'off',approvals,mock_model_requests:0,structured_reader_verified:true,elapsed_ms:Date.now()-started},null,2));
 }catch(error){if(page)await page.screenshot({path:join(report,'failure.png')}).catch(()=>{});await writeFile(join(report,'failure.txt'),fixture.redact(error));throw new Error(fixture.redact(error));}
finally {await cleanupAll(() => browser?.close(), () => fixture.close());}
