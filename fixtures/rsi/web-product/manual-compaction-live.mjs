import assert from 'node:assert/strict';
import { mkdir, readFile, writeFile } from 'node:fs/promises';
import { join, resolve } from 'node:path';
import { chromium } from 'playwright';
import { startService, waitUntil } from './service.mjs';
import { chooseStartingWorkspace, connectHostOwner, registerFixtureWorkspace } from './browser-fixture.mjs';
import { attemptEvents } from './model-attempt.mjs';
import { deepseekProfile, liveLimits, readDeepseekKey } from './live-limits.mjs';

const { RSI_WEB_BINARY: binary, RSI_WEB_ASSETS: assets, RSI_LIVE_ENV_FILE: env, RSI_WEB_REPORT: output } = process.env;
assert(binary && assets && env && output, 'explicit binary, assets, live credentials file and report directory required');
const report = resolve(output);
await mkdir(report, { recursive: true });
const key = await readDeepseekKey(env);
const model = process.env.RSI_LIVE_MODEL;
assert(model);
let browser, page, service;
const checks = [];
const errors = [];
try {
  service = await startService({ binary, assets, report, localWeb: true, developmentHttp: true, foregroundHost: process.platform === 'win32', deepseekKey: key,
    configure: async ({config}) => {
      await writeFile(join(config, 'settings.json'), JSON.stringify({'rsi.agent': {default_model: {deployment: 'live', model}}}));
      await writeFile(join(config, 'host-profiles/fixture/host.profile.toml'), deepseekProfile(model, liveLimits()));
    } });
  browser = await chromium.launch();
  page = await browser.newPage({ viewport: {width: 1440, height: 960}, colorScheme: 'dark' });
  page.on('pageerror', error => errors.push(error.message));
  const registration = service.register('manual summary verification');
  await registerFixtureWorkspace(page.request, service, registration);
  await connectHostOwner(page, service);
  await chooseStartingWorkspace(page, service);
  const editor = page.getByRole('textbox', {name: 'Main message', exact: true});
  const send = async text => {
    await editor.fill(text);
    await page.getByTestId('composer-send').click();
  };
  // The semantic planner protects the most recent 64 KiB. An older oversized
  // user message exercises a real eligible prefix while keeping model output small.
  await send('The archive code is ORCHID. Reply exactly FIRST_ACK. Ignore this test padding: ARCHIVED_PADDING_91 ' + 'a'.repeat(70_000));
  await page.getByText('FIRST_ACK', {exact: true}).last().waitFor({timeout: 180_000});
  const session = await page.locator('.pane').getAttribute('data-session-id');
  assert(session);
  const facts = () => service.readSessionFacts(session);
  const terminal = count => waitUntil(async () => (await facts()).filter(f => f.type === 'turn_terminal').length >= count, 'durable turn ' + count, 180_000);
  await terminal(1);
  await send('Keep the archive code available for later. Reply exactly SECOND_ACK.');
  await terminal(2);
  const transcript = page.getByRole('region', {name:'Conversation transcript'});
  await transcript.focus();await transcript.press('End');
  await send('/compact');
  await waitUntil(async () => (await facts()).some(f => f.type === 'manual_compaction' && f.event.stage === 'installed'), 'installed summary', 180_000);
  const compacted = await facts();
  const stages = compacted.filter(f => f.type === 'manual_compaction');
  assert.deepEqual(stages.map(f => f.event.stage), ['prepared','started','settled','installed']);
  assert.equal(new Set(stages.map(f => f.command_id)).size, 1);
  assert.equal(compacted.filter(f => f.type === 'turn_terminal').length, 2);
  assert(stages.every(f => f.turn_id == null));
  const textBlocks = new Map();
  for (const {event} of attemptEvents(stages[2].event.attempt)) {
    if (event.type === 'content_delta' && event.delta.type === 'text')
      textBlocks.set(event.index, (textBlocks.get(event.index) ?? '') + event.delta.value);
  }
  const summaryText = [...textBlocks.entries()].sort(([a],[b])=>a-b).map(([,text])=>text).join('');
  checks.push('four standalone stages with one command and no added turn');
  const marker = transcript.getByRole('button', {name:/compact.*Compacted conversation history/i});
  await marker.waitFor();
  assert.equal(await marker.isEnabled(),true);
  assert.equal(await transcript.locator('[data-compaction-preview]').count(),0);
  assert.equal(await page.getByText('Compacted conversation history.', {exact: true}).count(), 1);
  await page.getByRole('option', {name: /compact/}).waitFor({state: 'hidden'});
  await page.evaluate(()=>new Promise(resolve=>requestAnimationFrame(()=>requestAnimationFrame(resolve))));
  const bounds=await marker.boundingBox(),viewport=await transcript.boundingBox();
  assert(bounds.y >= viewport.y && bounds.y+bounds.height <= viewport.y+viewport.height,'the complete marker is visible above the composer');
  await page.screenshot({path: join(report, 'manual-compaction.png')});
  await marker.focus();
  await marker.press('Enter');
  const summary = page.getByRole('region',{name:'Resource workbench'}).locator('[data-compaction-preview]');
  await summary.getByText(/ORCHID/).first().waitFor();
  const renderedSummary = await summary.innerText();
  await summary.getByText(/ORCHID/).first().scrollIntoViewIfNeeded();
  await page.screenshot({path: join(report, 'manual-compaction-sidebar.png')});
  await marker.press('Space');
  const summaryTab=page.getByRole('tab',{name:/Context summary/});
  assert.equal(await summaryTab.count(),1);
  await page.setViewportSize({width:390,height:844});
  await summaryTab.waitFor();
  await page.screenshot({path:join(report,'manual-compaction-narrow.png')});
  const close=summaryTab.getByRole('button',{name:'Close resource tab'});
  await close.focus();await close.press('Enter');
  await summary.waitFor({state:'hidden'});
  await page.setViewportSize({width:1440,height:960});
  checks.push('one transcript marker opens the cited summary in one responsive sidebar tab and its keyboard close returns to chat');
  await send('What was the archive code? Reply only with that word.');
  await terminal(3);
  await page.getByText('ORCHID', {exact: true}).last().waitFor({timeout: 30_000});
  const ready = page.waitForEvent('download');
  await send('/export -f json -i h,lpr');
  const download = await ready;
  const exported = JSON.parse(await readFile(await download.path(), 'utf8'));
  assert.equal(exported.last_provider_request.availability, 'available');
  const messages = exported.last_provider_request.request.messages;
  assert(messages.some(message=>message.content.some(part=>part.type === 'text' && part.content.text.endsWith('\n'+summaryText))));
  assert(!JSON.stringify(messages).includes('a'.repeat(1000)));
  checks.push('live follow-up answered from installed summary and excluded original padding');
  // Trajectory exposes request metadata and its exact evidence selector.
  await page.getByText('Trajectory', {exact: true}).first().click();
  const ledger = page.getByRole('table', {name:'Trajectory records'});
  await ledger.getByRole('row').filter({has:page.getByRole('cell',{name:'Request',exact:true})}).last().getByRole('button').click();
  await page.getByRole('complementary',{name:'Event details'}).getByRole('button', {name:'Tool names and changes'}).click();
  await page.getByText('file_read', {exact:false}).last().waitFor({state:'visible',timeout:20_000});
  await page.screenshot({path: join(report, 'request-tools.png')});
  checks.push('selected actual request shows its tool names');
  await page.getByRole('tab',{name:'Chat',exact:true}).click();
  await marker.click();
  await summary.waitFor();
  await page.reload();
  await page.locator('#workbench').waitFor({state:'visible'});
  await marker.waitFor();
  assert.equal(await summaryTab.count(),0);
  await marker.click();
  assert.equal(await page.locator('.pane').getAttribute('data-session-id'),session);
  assert.equal(await summary.innerText(), renderedSummary);
  await summary.getByText(/ORCHID/).first().scrollIntoViewIfNeeded();
  assert.equal(await marker.count(), 1);
  assert.equal((await facts()).filter(f => f.type === 'manual_compaction').length, 4);
  await page.screenshot({path: join(report, 'manual-compaction-reopened.png')});
  checks.push('reload drops the transient tab and the same marker reopens identical summary content without a new paid stage');
  assert.deepEqual(errors, []);
  await writeFile(join(report, 'result.json'), JSON.stringify({ok:true, browser:browser.version(), checks,
    command:stages[0].command_id, effect:stages[0].effect_id, stage_sequences:stages.map(f=>f.seq), model, turn_count:3}, null, 2));
} catch (error) {
  await page?.screenshot({path:join(report, 'failure.png')}).catch(()=>{});
  await writeFile(join(report, 'result.json'), JSON.stringify({ok:false, checks, error:String(error.message).replaceAll(key,'[redacted]')},null,2));
  throw error;
} finally {
  await browser?.close();
  await service?.close();
}
