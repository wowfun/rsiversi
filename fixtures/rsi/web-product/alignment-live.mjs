import {disconnect} from './controls.mjs';
import {workspaceForm} from './controls.mjs';
// Explicit opt-in actual-provider acceptance for the DSH-aligned conversation.
import {paired} from './paired-env.mjs';
import assert from 'node:assert/strict';
import {mkdir,readFile,writeFile} from 'node:fs/promises';
import {join,resolve} from 'node:path';
import {createHash} from 'node:crypto';
import {chromium} from 'playwright';
import {startService} from './service.mjs';
import {detailMode} from './controls.mjs';
import {readDeepseekKey,liveLimits,deepseekProfile} from './live-limits.mjs';
import {requireProviderUsage,settleLiveEvidence} from './evidence.mjs';

assert(process.env.RSI_LIVE_ENV_FILE && process.env.RSI_LIVE_MODEL && process.env.RSI_WEB_REPORT,'Explicit live environment, model and report required');
const model=process.env.RSI_LIVE_MODEL,limits=liveLimits(),key=await readDeepseekKey(process.env.RSI_LIVE_ENV_FILE);
const report=resolve(process.env.RSI_WEB_REPORT);await mkdir(report,{recursive:false});
const original='RSI_READONLY_OK_1027\nalpha=7\nbeta=11\nRead this file only. Do not modify the workspace.\n';
const sha=bytes=>createHash('sha256').update(bytes).digest('hex');
let service,browser,page;const errors=[],checks=[];
try {
  service=await startService({binary:paired.binary,assets:paired.assets,report,deepseekKey:key,configure:async({config,workspace})=>{
    await writeFile(join(config,'host-profiles/fixture/host.profile.toml'),deepseekProfile(model,limits).replace('deployment="live"','deployment="live"\nprotocol="chat-completions"').replace('default="off"','default="high"'));
    await writeFile(join(config,'settings.json'),JSON.stringify({'rsi.agent':{default_model:{deployment:'live',model},output_budgets:[{model:{deployment:'live',model},max_output_tokens:limits.default_output_reserve_tokens}]}}));
    await writeFile(join(workspace,'live-check.txt'),original);
  }});
  browser=await chromium.launch();const context=await browser.newContext({ignoreHTTPSErrors:true,viewport:{width:1440,height:900}});
  page=await context.newPage();page.setDefaultTimeout(30000);page.on('pageerror',error=>errors.push(error.message));
  await page.goto(service.origin);await page.getByLabel('Device registration receipt').fill(JSON.stringify(service.register('alignment live')));
  await page.getByRole('button',{name:'Connect',exact:true}).click();await page.locator('#workbench').waitFor();
  await workspaceForm(page);await page.getByLabel('Directory path').fill(service.workspace);
  await page.locator('.directory-picker footer').getByRole('button',{name:'Open',exact:true}).click();await page.locator('#workspaces [data-testid=workspace-open]').click();
  const pane=page.getByRole('region',{name:'Main conversation',exact:true}),input=pane.getByLabel('Main message',{exact:true});
  const submit=async prompt=>{await input.fill(prompt);await pane.getByTestId('composer-send').click();await page.waitForFunction(()=>document.querySelector('[aria-label="Main message"]').value==='');};
  const complete=async(marker,stage)=>{
    await pane.locator('.message.assistant').filter({hasText:marker}).waitFor({timeout:240000});
    await pane.locator('.pane-status').filter({hasText:'Completed'}).waitFor({timeout:240000});
    const session=await pane.getAttribute('data-session-id'),facts=await service.readSessionFacts(session);
    const usage=requireProviderUsage(facts);await writeFile(join(report,`${stage}-facts.json`),JSON.stringify(facts,null,2));
    await writeFile(join(report,`${stage}-transcript.txt`),await pane.locator('.transcript').innerText());
    await page.screenshot({path:join(report,`${stage}.png`)});return {session,facts,usage};
  };
  await submit('Use no tools. Return a small Markdown table with columns Item and Value and rows alpha=7 and beta=11; a fenced js code block containing const sum = 7 + 11; and a final line exactly RSI_MARKDOWN_OK_1027.');
  const markdown=await complete('RSI_MARKDOWN_OK_1027','markdown');
  assert(await pane.locator('table').count()>0);assert(await pane.locator('pre code').count()>0);
  checks.push({markdown:true,table:true,code:true,usage:markdown.usage});
  await detailMode(page,'verbose');
  await submit('Use only the file_read tool to read live-check.txt in the current workspace. Do not write any file or run shell commands. Report its first line, alpha and beta, then the exact marker RSI_READ_VERIFIED_1027.');
  const read=await complete('RSI_READ_VERIFIED_1027','readonly');
  assert(read.facts.some(fact=>fact.type==='tool_intent'&&fact.name==='file_read'),'actual file_read intent required');
  assert.equal((await readFile(join(service.workspace,'live-check.txt'))).toString(),original);
  checks.push({readonly:true,fileSha256:sha(original),usage:read.usage});
  await submit('Use no tools. Write 500 numbered, distinct UI test cases, each with a full sentence. Do not summarize; continue until all 500 are written.');
  const stop=pane.getByRole('button',{name:'Stop',exact:true});await stop.waitFor({timeout:60000});await stop.click();
  await stop.waitFor({state:'hidden',timeout:60000});
  const stopped=await service.readSessionFacts(await pane.getAttribute('data-session-id'));
  assert.equal(stopped.findLast(fact=>fact.type==='turn_terminal')?.outcome.status,'cancelled','durable cancellation outcome required');
  await writeFile(join(report,'stop-facts.json'),JSON.stringify(stopped,null,2));
  await page.screenshot({path:join(report,'stopped.png')});
  checks.push({stopClicked:true,stopHiddenAfterSettlement:true,durableOutcome:'cancelled'});
  await submit('Use no tools. Reply with exactly RSI_STOP_RECOVERY_OK_1027.');
  const recovery=await complete('RSI_STOP_RECOVERY_OK_1027','recovery');
  assert.equal(service.provider.requests.length,0,'no deterministic provider requests permitted');
  assert.equal((await readFile(join(service.workspace,'live-check.txt'))).toString(),original);
  await disconnect(page);await page.locator('#login').waitFor();assert.deepEqual(errors,[]);
  await writeFile(join(report,'result.json'),JSON.stringify({status:'passed',model,protocol:'chat-completions',limits,browser:browser.version(),checks,recovery:true,session:recovery.session,mockRequests:0,errors},null,2));
} catch(error) {
  await page?.screenshot({path:join(report,'failure.png')}).catch(()=>{});
  await writeFile(join(report,'failure.json'),JSON.stringify({error:String(error),checks,errors},null,2).replaceAll(key,'[REDACTED]'));
  throw new Error(String(error).replaceAll(key,'[REDACTED]'));
} finally {await settleLiveEvidence(report,key,()=>browser?.close(),()=>service?.close());}
