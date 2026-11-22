import assert from 'node:assert/strict';
import {copyFile, mkdir, readFile, writeFile} from 'node:fs/promises';
import {basename, join, resolve} from 'node:path';
import {chromium} from 'playwright';
import {chooseStartingWorkspace, connectHostOwner, registerFixtureWorkspace} from './browser-fixture.mjs';
import {navigationMode, openConversation} from './controls.mjs';
import {startService, waitUntil} from './service.mjs';

const {RSI_WEB_BINARY: binary, RSI_WEB_ASSETS: assets, RSI_WEB_REPORT: output} = process.env;
assert(binary && assets && output, 'Select a Web distribution and a fresh report directory');
const report = resolve(output);
await mkdir(report);
const name = 'tool-history';
const base = 'format = 1\n[[steps]]\nkind = "include"\npath = "base.toml"\n';
const disabled = '[[steps]]\nkind = "patch"\ntarget = "external-agents"\nenabled = false\n';
const result = {received: [], displayed: [], errors: []};
let service, browser, page, profile, session;
try {
  service = await startService({binary, assets, report, localWeb: true, developmentHttp: true,
    foregroundHost: process.platform === 'win32',
    async configure({config}) {
      const dir = join(config, 'agent-presets', name);
      await mkdir(dir, {recursive: true});
      profile = join(dir, 'agent.profile.toml');
      await copyFile(new URL('../../../crates/rsi/core/presets/standard/agent.profile.toml', import.meta.url), join(dir, 'base.toml'));
      await writeFile(join(dir, 'preset.toml'), 'name = "Evidence probe"\n');
      await writeFile(profile, base);
    },
    onRequest(body) { result.received.push(body.tools.map(tool => tool.function)); },
  });
  function inspectPreset() {
    const {origins} = JSON.parse(service.run(['agent-preset', 'show', name, '--output', 'json']).stdout);
    const node = origins.nodes['external-agents'];
    return {declaration: basename(origins.layers[node.declared_by].source),
      enabledFrom: basename(origins.layers[node.enabled_from].source), disabledBy: node.disabled_by};
  }
  result.initialPreset = inspectPreset();
  assert.deepEqual(result.initialPreset, {declaration: 'base.toml', enabledFrom: 'base.toml', disabledBy: null});
  browser = await chromium.launch();
  page = await browser.newPage({viewport: {width: 1440, height: 960}});
  page.on('pageerror', error => result.errors.push(error.message));
  await registerFixtureWorkspace(page.request, service, service.register('Request evidence fixture'));
  await connectHostOwner(page, service);
  const headers = {'x-rsi-csrf': '1', 'x-rsi-wire-version': '2', origin: service.origin};
  const description = await page.request.post(service.origin+'/api/v2/rsi-api-protocol/5/connection/describe/1/read',
    {headers, data: {wire_version: 2}});
  assert.equal(description.status(), 200);
  headers['x-rsi-host-epoch'] = description.headers()['x-rsi-host-epoch'];
  const acp = JSON.parse(await readFile(new URL('../../../crates/rsi-acp/api/wire-contract.json', import.meta.url)));
  const endpoints = await page.request.post(service.origin+'/api/v2/'+acp.name+'/'+acp.version+'/external-conversation/endpoints/1/read',
    {headers, data: {}});
  assert.equal(endpoints.status(), 200);
  result.endpoints = await endpoints.json();
  assert.deepEqual(result.endpoints, []);
  await chooseStartingWorkspace(page, service);
  const pane = page.locator('.pane.selected');
  await pane.locator('button.preset-choice').click();
  await page.getByRole('menuitemradio', {name: /Evidence probe/}).click();
  const editor = pane.getByRole('textbox', {name: 'Main message'});
  async function send(text, count) {
    await editor.fill(text);
    await page.getByTestId('composer-send').click();
    await pane.getByText('Reviewed: '+text, {exact: true}).waitFor();
    session ??= await pane.getAttribute('data-session-id');
    await waitUntil(async () => (await service.readSessionFacts(session)).filter(fact => fact.type === 'turn_terminal').length === count, 'settled turn');
  }
  async function reopen(change) {
    await service.restart(change);
    await connectHostOwner(page, service);
    await navigationMode(page, 'flat');
    if (await pane.getAttribute('data-session-id') !== session) await openConversation(page, pane, session);
    assert.equal(await pane.getAttribute('data-session-id'), session);
  }
  await send('First evidence sample', 1);
  await reopen(() => writeFile(profile, base+disabled));
  result.changedPreset = inspectPreset();
  assert.deepEqual(result.changedPreset, {declaration: 'base.toml', enabledFrom: 'agent.profile.toml', disabledBy: 'external-agents'});
  await send('Second evidence sample', 2);
  await send('Repeated evidence sample', 3);
  await reopen();
  assert.equal(result.received.length, 3);
  const names = result.received.map(tools => tools.map(tool => tool.name).sort());
  assert.deepEqual(names[0].filter(tool => !names[1].includes(tool)), ['external_agent']);
  assert.deepEqual(names[1].filter(tool => !names[0].includes(tool)), []);
  assert.deepEqual(result.received[1], result.received[2]);
  const facts = await service.readSessionFacts(session);
  assert.equal(facts.filter(fact => fact.type === 'tool_intent').length, 0);
  const requests = facts.filter(fact => fact.type === 'model_intent');
  const intents = requests.map(fact => fact.seq);
  assert.equal(intents.length, 3);
  result.toolSections = requests.map(({seq, evidence}) => ({intent: seq, kind: evidence.tools.kind,
    source: evidence.tools.seq ?? seq, sha256: evidence.tools.sha256}));
  assert.deepEqual(result.toolSections.map(part => part.kind), ['inline', 'inline', 'reference']);
  assert.equal(result.toolSections[2].source, intents[1]);
  assert.equal(result.toolSections[2].sha256, result.toolSections[1].sha256);
  await page.getByText('Trajectory', {exact: true}).first().click();
  const rows = page.getByRole('table', {name: 'Trajectory records'}).getByRole('row')
    .filter({has: page.getByRole('cell', {name: 'Request', exact: true})});
  await rows.nth(2).waitFor();
  assert.equal(await rows.count(), 3);
  const details = page.getByRole('complementary', {name: 'Event details'});
  const tabs = page.locator('[data-dockkit-tab-title]');
  async function readSource(seq, field, action) {
    await details.getByRole('button', {name: action, exact: true}).click();
    const source = page.locator('.source-detail:visible').filter({hasText: 'Fact '+seq+' · '+field});
    const content = source.locator('.source-text');
    await content.waitFor();
    const title = seq+' · '+(field === 'model_tool_summary' ? 'Tools' : 'Schemas');
    assert.ok((await tabs.allTextContents()).includes(title), 'missing evidence tab '+title);
    const pages = [];
    while (true) {
      const range = source.locator('.source-range');
      const previous = await range.innerText();
      pages.push(await content.textContent());
      const next = source.getByRole('button', {name: 'Next source page'});
      if (await next.isDisabled()) break;
      await next.click();
      await waitUntil(async () => await range.innerText() !== previous, 'next evidence page');
    }
    return pages.join('');
  }
  for (const [index, seq] of intents.entries()) {
    await rows.nth(index).getByRole('button').click();
    const summary = await readSource(seq, 'model_tool_summary', 'Tool names and changes');
    const [heading, advertised] = summary.split('\n');
    assert.equal(heading, 'Request '+seq+' · '+names[index].length+' advertised functions');
    assert.deepEqual(advertised.split(', ').sort(), names[index]);
    if (index === 0) assert.match(summary, /First recorded request of this kind/);
    else {
      assert.ok(summary.includes('Compared with request '+intents[index-1]));
      assert.ok(summary.includes(index === 1 ? 'Removed: external_agent' : 'same tool definitions and choice'));
    }
    await page.screenshot({path: join(report, 'tools-'+seq+'.png')});
    const {definitions} = JSON.parse(await readSource(seq, 'model_tool_schemas', 'Full tool schemas and choice'));
    assert.deepEqual(definitions.map(({name, description, input_schema}) => ({name, description, parameters: input_schema})), result.received[index]);
    result.displayed.push({seq, summary, matchedSchemas: definitions.length});
  }
  await tabs.filter({hasText: new RegExp('^'+intents[0]+' · Tools$')}).click();
  const original = page.locator('.source-detail:visible').filter({hasText: 'Fact '+intents[0]+' · model_tool_summary'});
  assert.match(await original.locator('.source-text').innerText(), /external_agent/);
  assert.deepEqual(result.errors, []);
  result.ok = true;
} catch (error) {
  result.error = error.message;
  if (page) await writeFile(join(report, 'screen.txt'), await page.locator('body').ariaSnapshot());
  throw error;
} finally {
  await writeFile(join(report, 'result.json'), JSON.stringify(result, null, 2));
  await browser?.close();
  await service?.close();
}
