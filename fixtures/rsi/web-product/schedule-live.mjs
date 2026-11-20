import assert from 'node:assert/strict';
import {appendFile, readFile, writeFile} from 'node:fs/promises';
import {basename,join} from 'node:path';
import {setTimeout as delay} from 'node:timers/promises';
import {chromium} from 'playwright';
import {assistantText} from './model-attempt.mjs';
import {openConversation, setSessionArchived} from './controls.mjs';
import {openBrowserPage, connectWorkbench, chooseStartingWorkspace, registerFixtureWorkspace} from './browser-fixture.mjs';
import {cleanupAll} from './cleanup.mjs';
import {liveFixture} from './live-fixture.mjs';

let hostProfile;
const fixture = await liveFixture({foregroundHost:true,developmentHttp:process.platform === 'win32',
  configure:async({config})=>{hostProfile=join(config,'host-profiles/fixture/host.profile.toml');}});
const {service, report, model} = fixture;
const crashHost = process.argv.includes('--crash-host');
const pendingRestart = process.argv.includes('--pending-restart');
const withoutNavigation = pendingRestart || process.argv.includes('--without-navigation');
const archived = withoutNavigation || process.argv.includes('--archive');
const errors = [];
let page, browser;

async function terminalFacts(session, expectedCount) {
  const deadline = Date.now() + 120_000;
  do {
    const facts = await service.readSessionFacts(session);
    if (facts.filter(fact => fact.type === 'turn_terminal').length >= expectedCount) return facts;
    await delay(250);
  } while (Date.now() < deadline);
  throw new Error('Scheduled session did not reach the expected terminal turn');
}

async function reconnect() {
  page = await browser.contexts()[0].newPage();
  page.on('pageerror', error => errors.push(error.message));
  await page.goto(service.origin);
  if (service.developmentHttp) await page.getByLabel('Allow local HTTP for development', {exact:true}).check();
  await page.getByRole('button',{name:'Reconnect with this browser',exact:true}).click();
  await page.locator('#workbench').waitFor({state:'visible'});
}

async function verify() {
  ({browser, page} = await openBrowserPage(chromium, errors));
  const registration=service.register('schedule creation');
  await registerFixtureWorkspace(page.request,service,registration);
  await connectWorkbench(page, service, registration);
  await chooseStartingWorkspace(page, service);
  const pane = page.getByRole('region', {name: 'Main conversation', exact: true});
  await pane.getByRole('textbox', {name: 'Main message'}).fill(
    'Create one reminder using schedule_create exactly once. Use rule {"kind":"after","delay_ms":45000} and the following prompt verbatim: "Call schedule_list once, then answer LIVE_SCHEDULE_ONCE. Do not change any reminders or files." Once creation succeeds, answer REMINDER_CREATED immediately.'
  );
  await pane.getByTestId('composer-send').click();
  await pane.locator('.message.assistant').first().waitFor();
  const session = await pane.getAttribute('data-session-id');
  const initial = await terminalFacts(session, 1);
  await writeFile(join(report, 'facts.json'), JSON.stringify(initial, null, 2));
  const creates = initial.filter(fact => fact.type === 'tool_intent' && fact.name === 'schedule_create');
  assert.equal(creates.length, 1);
  const creation = initial.find(fact => fact.type === 'tool_result' && fact.turn_id === creates[0].turn_id && fact.effect_id === creates[0].effect_id)?.result.value;
  assert.equal(creation?.status, 'created_armed');
  assert.equal(creation.state.reminders.length, 1);
  const reminder = creation.state.reminders[0];
  const createdCard = pane.locator('[data-turn-tail]').getByRole('button', {name:'Open automation task: '+reminder.prompt.split('\n',1)[0]});
  await createdCard.waitFor();
  await page.screenshot({path:join(report,'created-task.png')});
  if (process.argv.includes('--delete-before-due')) {
    await page.getByRole('button',{name:'Automation tasks',exact:true}).click();
    const manager = page.getByRole('region',{name:'Automation tasks',exact:true});
    const row = manager.getByRole('button',{name:reminder.prompt.split('\n',1)[0],exact:true});
    await row.click();
    await manager.getByRole('button',{name:'Automation task actions'}).click();
    await page.getByRole('menuitem',{name:'Delete task'}).click();
    const confirmation = page.getByRole('dialog',{name:'Delete this task?'});
    await page.screenshot({path:join(report,'delete-confirmation.png')});
    await confirmation.getByRole('button',{name:'Confirm deletion'}).click();
    await row.waitFor({state:'detached',timeout:8000});
    const deletedAt = Date.now();
    assert(deletedAt < reminder.due_ms, 'deletion must finish before the first delivery');
    await page.close();
    page = undefined;
    await service.restart();
    await reconnect();
    await page.getByRole('button',{name:'Automation tasks',exact:true}).click();
    await page.getByText('No active automation tasks',{exact:true}).waitFor();
    await delay(Math.max(0,reminder.due_ms - Date.now() + 1000));
    const after = await service.readSessionFacts(session);
    assert.deepEqual(after,initial);
    assert.deepEqual(errors,[]);
    await writeFile(join(report,'result.json'),JSON.stringify({ok:true,model,browser:browser.version(),session,
      reminder:reminder.id,deletedAt,due:reminder.due_ms,observedAfterRestart:Date.now(),retainedFacts:after.length,
      providerTurns:after.filter(f=>f.type==='turn_terminal').length},null,2));
    return;
  }
  if (archived) {
    await setSessionArchived(page, session, true);
    await page.getByRole('button',{name:'Automation tasks',exact:true}).click();
    const manager = page.getByRole('region',{name:'Automation tasks',exact:true});
    await manager.getByRole('button',{name:reminder.prompt.split('\n',1)[0],exact:true}).click();
    const detail = manager.getByRole('complementary',{name:'Selected task',exact:true});
    await detail.getByText('The linked session is archived.',{exact:true}).waitFor();
    assert(await detail.getByRole('button',{name:/open linked session$/i}).isDisabled());
    await page.screenshot({path:join(report,'archived-linked-session.png')});
  }
  const originalPid = service.foregroundPids[0];
  let originalProfile;
  async function disableNavigation() {
    originalProfile = await readFile(hostProfile);
    await appendFile(hostProfile,"\n[[steps]]\nkind='patch'\ntarget='rsi.navigation'\nenabled=false\n");
  }
  if (pendingRestart) {
    await disableNavigation();
    const reload = JSON.parse(service.run(['host','reload']).stdout);
    assert.equal(reload.outcome,'restart_required');
    assert(service.run(['host','status']).stdout.split('\t').includes('pid='+originalPid));
    await delay(reminder.due_ms - Date.now() + 750);
    const facts = await service.readSessionFacts(session);
    assert.equal(facts.filter(fact => fact.type === 'turn_terminal').length, 1);
    assert(!facts.some(fact => fact.type === 'input_message_claimed' && fact.source?.type === 'continuation'));
    await page.getByText('The linked session is archived.',{exact:true}).waitFor();
    await writeFile(join(report,'pending-restart.json'), JSON.stringify({reload,originalPid,due:reminder.due_ms,observed:Date.now(),lastSeq:facts.at(-1).seq}));
  }
  await page.close();
  page = undefined;
  await service.restart(async () => {
    if (pendingRestart) return;
    assert(Date.now() < reminder.due_ms, 'original Host must have stopped before the deadline');
    if (withoutNavigation) await disableNavigation();
    await delay(reminder.due_ms - Date.now() + 25);
  }, {crashHost});
  const restartedPid = service.foregroundPids[0];
  assert.notEqual(restartedPid, originalPid);
  if (archived && !withoutNavigation) {
    await reconnect();
    const beforeRestore = await service.readSessionFacts(session);
    assert(Date.now() > reminder.due_ms);
    assert.equal(beforeRestore.filter(fact => fact.type === 'turn_terminal').length, 1);
    assert(!beforeRestore.some(fact => fact.type === 'input_message_claimed' && fact.source?.type === 'continuation'));
    await writeFile(join(report,'before-restore.json'), JSON.stringify({due:reminder.due_ms, observed:Date.now(),lastSeq:beforeRestore.at(-1).seq}));
    await setSessionArchived(page, session, false);
  }
  const facts = await terminalFacts(session, 2);
  await writeFile(join(report, 'facts.json'), JSON.stringify(facts, null, 2));
  const claimed = facts.filter(fact => fact.type === 'input_message_claimed' && fact.source?.type === 'continuation');
  assert.equal(claimed.length, 1);
  const turn = claimed[0].turn_id;
  assert.equal(claimed[0].source.source.domain.id, 'rsi.schedule');
  const list = facts.find(fact => fact.type === 'tool_intent' && fact.turn_id === turn && fact.name === 'schedule_list');
  assert(list, 'scheduled turn must inspect the same reminder');
  const result = facts.find(fact => fact.type === 'tool_result' && fact.turn_id === turn && fact.effect_id === list.effect_id)?.result.value;
  assert.equal(result?.state.allocated_rounds, 1);
  assert.equal(result.state.reminders[0].id, reminder.id);
  assert(result.state.reminders[0].consumed);
  assert.match(assistantText(facts.filter(fact => fact.turn_id === turn)), /LIVE_SCHEDULE_ONCE/);
  assert.equal(service.provider.requests.length, 0);

  if (withoutNavigation) {
    await service.restart(() => writeFile(hostProfile, originalProfile));
    await reconnect();
    await setSessionArchived(page, session, false);
  }
  if (!page) await reconnect();
  const reopened = page.getByRole('region', {name: 'Main conversation', exact: true});
  if (!archived) {
    const group = page.locator('#workspaces .workspace-toggle').filter({has:page.getByText(basename(service.workspace),{exact:true})});
    if (await group.getAttribute('aria-expanded') === 'false') await group.click();
  }
  await openConversation(page, reopened, session);
  await reopened.locator('.message.assistant').filter({hasText: 'LIVE_SCHEDULE_ONCE'}).waitFor();
  const trigger = reopened.locator('.turn-trigger').filter({has:page.getByText('Automation task',{exact:true})});
  const toggle = trigger.getByRole('button',{name:/Automation task/});
  await toggle.waitFor();
  assert.equal(await toggle.getAttribute('aria-expanded'),'false');
  assert.equal(await trigger.locator('.message-text').count(),0);
  await page.screenshot({path: join(report, 'schedule-wide.png')});
  await toggle.focus();
  await page.keyboard.press('Enter');
  await trigger.locator('.message-text').filter({hasText:'LIVE_SCHEDULE_ONCE'}).waitFor();
  await page.screenshot({path:join(report,'notification-expanded.png')});
  await page.keyboard.press('Space');
  assert.equal(await toggle.getAttribute('aria-expanded'),'false');
  await page.setViewportSize({width: 420, height: 900});
  await reopened.locator('.transcript').evaluate(node => { node.scrollTop = node.scrollHeight; });
  await page.screenshot({path: join(report, 'schedule-narrow.png')});
  if (withoutNavigation) {
    const restored = await service.readSessionFacts(session);
    assert.deepEqual(restored.filter(fact => fact.type === 'input_message_claimed' && fact.source?.type === 'continuation'),claimed);
  }
  assert.deepEqual(errors, []);
  await writeFile(join(report, 'result.json'), JSON.stringify({
    ok: true, browser: browser.version(), model, platform: process.platform, originalPid, restartedPid,
    hostStop:crashHost?'process-termination':'graceful',
    session, reminder: reminder.id, automaticRounds: result.state.allocated_rounds, terminalBeforeReconnect: !archived || withoutNavigation,
    ...(withoutNavigation ? {archivedBeforeRestart:true,deliveredWithNavigationDisabled:true,singleClaimAfterNavigationRestored:true}
      : archived ? {deferredUntilUnarchive:true, terminalBeforeSessionReopened:true} : {}),
    ...(pendingRestart ? {deferredAcrossReloadUntilHostRestart:true} : {}),
    transcriptAttribution:'Automation task', triggerCollapsedOnReopen:true, expandedOriginalInput:true, createdTaskInTurnTail:true,
  }, null, 2));
}
try {
  await verify();
} catch (error) {
  await page?.screenshot({path: join(report, 'failure.png')}).catch(() => {});
  const failure = fixture.redact(error.stack ?? error);
  await writeFile(join(report, 'failure.txt'), failure);
  throw new Error(failure);
} finally {
  await cleanupAll(() => browser?.close(), () => fixture.close());
}
