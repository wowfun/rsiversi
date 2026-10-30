import {disconnect} from './controls.mjs';
import {workspaceForm} from './controls.mjs';
import {liveLimits,readDeepseekKey,deepseekProfile} from './live-limits.mjs';
import {assistantText} from './model-attempt.mjs';
import {detailMode} from './controls.mjs';
import './paired-env.mjs';
// Opt-in real model acceptance; this file is never loaded by default tests.
import assert from 'node:assert/strict';
import {readFile,writeFile,mkdir} from 'node:fs/promises';
import {join,resolve} from 'node:path';
import {chromium,firefox} from 'playwright';
import {startService,waitUntil,boundedRun} from './service.mjs';
import {liveTurnTerminal,settleLiveEvidence} from './evidence.mjs';
import {browserNames} from './task-checks.mjs';
const report=process.env.RSI_WEB_REPORT,model=process.env.RSI_LIVE_MODEL;
assert(report&&model&&process.env.RSI_LIVE_ENV_FILE&&process.env.RSI_WEB_ASSETS&&process.env.RSI_WEB_BINARY,'explicit live inputs required');
const limits=liveLimits();
assert(/^[a-zA-Z0-9_.-]{1,128}$/.test(model));
const key=await readDeepseekKey(process.env.RSI_LIVE_ENV_FILE);
await mkdir(report,{recursive:true});const binary=process.env.RSI_WEB_BINARY;
const tokens=['ROLE_SOURCE_V1_f387e5','ROLE_SOURCE_V2_9ab12c'];
const definition=version=>`---\ndescription: Give a concise isolated review\nmodel: { deployment: live, model: ${model} }\nreasoning_effort: off\nallow: []\n---\nYou are the isolated review subagent. Return exactly ${tokens[version-1]} followed by one short sentence reviewing the supplied task. You have no tools.\n`;
const html='<!doctype html><html><head><meta charset="utf-8"><style>body{font-family:sans-serif;padding:32px;background:#edf7f1;color:#0b5149}button{padding:12px;font-size:20px}</style></head><body><h1>Live model artifact</h1><p>Generated through the real coding tools.</p><button onclick="this.textContent=\'Clicked successfully\'">Click to verify</button></body></html>';
const browserName=browserNames(process.env.RSI_WEB_BROWSER??'chromium')[0];
const browser=await ({chromium,firefox})[browserName].launch();let service,page;const errors=[];let approvals=0;
function facts(session) {
 const records=service.run(["run",'evidence-cli','--resume',session,'--output','jsonl'],{input:':history\n'.repeat(32)+':exit\n'}).stdout.trim().split('\n').map(line=>JSON.parse(line));
 return records.flatMap(record=>record.fact?[record.fact]:[]).sort((a,b)=>a.seq-b.seq);
}
async function complete(pane,input,prompt) {
 const previous=await pane.getAttribute('data-session-id');
 const afterSeq=previous&&['Completed','Failed'].includes(await pane.locator('.pane-status').innerText())?(facts(previous).at(-1)?.seq??0):0;
 await input.fill(prompt);await pane.getByTestId('composer-send').click();
 const expires=Date.now()+180000;
 while(Date.now()<expires){
  const status=await pane.locator('.pane-status').innerText();
  const review=pane.locator('.pending button').filter({hasText:'Review:'});
  if(await review.count()&&await review.first().isVisible()){await review.first().click();await page.getByRole('button',{name:'Allow once',exact:true}).click();approvals++;}
  if(['Completed','Failed'].includes(status)){
   const session=await pane.getAttribute('data-session-id');
   const terminal=session&&liveTurnTerminal(facts(session),prompt,afterSeq);
   if(terminal){assert.equal(terminal.outcome.status,'completed');assert.equal(status,'Completed',(await pane.locator('.transcript').innerText()).slice(-4096));return;}
  }
  await page.waitForTimeout(100);
 }
 throw new Error('Live turn exceeded 180 seconds');
}
try {
 service=await startService({binary,assets:process.env.RSI_WEB_ASSETS,report,deepseekKey:key,configure:async({config,workspace})=>{
  boundedRun('git',['init','--quiet',workspace]);
  await writeFile(join(config,'settings.json'),JSON.stringify({'rsi.agent':{default_model:{deployment:'live',model},default_reasoning_effort:'off'}}));
  await writeFile(join(config,'host-profiles/fixture/host.profile.toml'),deepseekProfile(model,limits));
  const cli=join(config,'application-profiles/evidence-cli');await mkdir(cli,{recursive:true});await writeFile(join(cli,'application.profile.toml'),'format=1\n[[steps]]\nkind="plugin"\nid="connection"\nplugin="rsi.application.connection"\nconfig={host_profile="fixture"}\n[[steps]]\nkind="plugin"\nid="cli"\nplugin="rsi.application.cli"\n');
  await mkdir(join(workspace,'.agents/agents'),{recursive:true});await writeFile(join(workspace,'.agents/agents/live-reviewer.md'),definition(1));
 }});
 const context=await browser.newContext({ignoreHTTPSErrors:true,viewport:{width:1440,height:980}});page=await context.newPage();page.setDefaultTimeout(30000);page.on('pageerror',error=>errors.push(error.message));
 await page.goto(service.origin);await page.locator('#receipt').fill(JSON.stringify(service.register('preview live verification')));await page.getByRole('button',{name:'Connect',exact:true}).click();await page.locator('#workbench').waitFor({state:'visible'});
 await workspaceForm(page);await page.getByLabel('Directory path').fill(service.workspace);await page.locator('.directory-picker footer').getByRole('button',{name:'Open',exact:true}).click();await page.locator('#workspaces [data-testid=workspace-open]').first().click();await detailMode(page,'verbose');
 const main=page.getByRole('region',{name:'Main conversation',exact:true}),input=main.getByRole('textbox',{name:'Main message',exact:true});
 await input.fill('@live');await main.getByRole('option').filter({hasText:'live-reviewer'}).waitFor();await main.getByRole('button',{name:'Preview agent',exact:true}).click();await main.locator('.resource-preview-text').filter({hasText:tokens[0]}).waitFor();await page.screenshot({path:join(report,'live-agent-definition.png')});await main.getByRole('button',{name:'Close preview',exact:true}).click();await input.press('Escape');
 const children=[];let parent;
 for(let version=1;version<=2;version++){
  if(version===2)await writeFile(join(service.workspace,'.agents/agents/live-reviewer.md'),definition(2));
  await complete(main,input,`@live-reviewer This is isolated acceptance phase ${version}. Use spawn_agent exactly once with role live-reviewer and task_name phase-${version}, message "Review whether a pure function returning 42 is deterministic". Omit fork_turns, model and reasoning_effort so the role defaults and completed history apply. Use wait_agent if needed to observe completion. Do not create other children or edit files. Quote the child's entire reply verbatim, including its role/source identifier, then summarize its result.`);
  parent=(await main.getAttribute('data-session-id'));
  let parentFacts=facts(parent);const intent=parentFacts.find(fact=>fact.type==='tool_intent'&&fact.name==='spawn_agent'&&fact.arguments.task_name===`phase-${version}`);assert(intent,'live spawn ToolIntent absent');
  const result=parentFacts.find(fact=>fact.type==='tool_result'&&fact.effect_id===intent.effect_id);assert(result&&!result.result.is_error,'live spawn failed');const child=result.result.value.session_id;assert(child);
  let childFacts;await waitUntil(()=>{childFacts=facts(child);return childFacts.some(fact=>fact.type==='turn_terminal')},'live child completion',120000);
  await writeFile(join(report,`child-${version}-facts.json`),JSON.stringify(childFacts,null,2));
  const answer=assistantText(childFacts);
  assert(answer.includes(tokens[version-1]),'child did not produce the current definition token');
  assert.equal(childFacts.filter(fact=>fact.type==='tool_intent').length,0,'empty allow child used tools');
  children.push(child);
  await page.screenshot({path:join(report,`live-agent-phase-${version}.png`)});
 }
 await complete(main,input,`Use bash to write exactly the following UTF-8 single-page HTML to live-preview.html in the current workspace, then call present for live-preview.html with description "Interactive live artifact". Do not modify any other file. HTML:\n${html}\nFinally reply LIVE_PREVIEW_READY.`);
 assert.equal((await readFile(join(service.workspace,'live-preview.html'),'utf8')).trimEnd(),html);
 await main.locator('article.message').filter({has:page.locator('.message-title').filter({hasText:/^present ·/})}).scrollIntoViewIfNeeded();
 await main.getByRole('button',{name:'Open current file',exact:true}).click();const frame=page.frameLocator('iframe.file-html');await frame.getByRole('heading',{name:'Live model artifact'}).waitFor();await frame.getByRole('button',{name:'Click to verify'}).click();await frame.getByRole('button',{name:'Clicked successfully'}).waitFor();await page.screenshot({path:join(report,'live-html-interactive.png')});
 const parentFacts=facts(parent);await writeFile(join(report,'parent-facts.json'),JSON.stringify(parentFacts,null,2));
 const parentAnswer=assistantText(parentFacts);
 for(const [index,token] of tokens.entries()){
  const completion=parentFacts.find(fact=>fact.type==='input_message_entered'&&fact.source?.type==='completion'&&fact.source.child_session_id===children[index]&&fact.source.outcome.status==='completed');
  assert(completion?.content.some(part=>part.type==='text'&&part.text.includes(token)),'durable child completion did not carry the current reply');
  assert(parentAnswer.includes(token),'main agent did not quote the child reply');
 }
 assert.equal(service.provider.requests.length,0);assert.deepEqual(errors,[]);assert.equal(await page.locator('#notice').innerText(),'');
 await disconnect(page);await page.locator('#login').waitFor({state:'visible'});
 await writeFile(join(report,'result.json'),JSON.stringify({status:'passed',model,parent,children,definition_versions:2,next_spawn_refresh:true,empty_allow:true,real_tool_created_html:true,interactive_preview:true,mock_model_requests:0,approvals,browser_name:browserName,browser:browser.version()},null,2));
}catch(error){if(page){await page.screenshot({path:join(report,'failure.png')}).catch(()=>{});await writeFile(join(report,'failure.html'),(await page.content().catch(()=>'' )).replaceAll(key,'[REDACTED]'));}throw new Error(String(error).replaceAll(key,'[REDACTED]'));}
finally{
 await settleLiveEvidence(report,key,()=>browser.close(),()=>service?.close());
}
