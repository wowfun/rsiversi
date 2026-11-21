import {workspaceForm} from './controls.mjs';
import {detailMode} from './controls.mjs';
import assert from 'node:assert/strict';
import {readFile} from 'node:fs/promises';
// Shared public-browser setup; probe assertions and service lifetime stay with callers.
export async function openBrowserPage(engine, errors) {
  const browser = await engine.launch();
  try {
    const context = await browser.newContext({ignoreHTTPSErrors: true, viewport: {width: 1440, height: 980}});
    const page = await context.newPage();
    page.setDefaultTimeout(30000);
    page.on('pageerror', error => errors.push(error.message));
    return {browser, page};
  } catch (error) {
    try { await browser.close(); } catch (cleanup) { throw new AggregateError([error, cleanup], 'Browser setup and cleanup failed'); }
    throw error;
  }
}
export async function connectWorkbench(page, service, registration) {
  await page.goto(service.origin);
  if (typeof registration === 'string') registration = service.register(registration);
  await page.locator('#receipt').fill(JSON.stringify(registration));
  if (service.developmentHttp) await page.getByLabel('Allow local HTTP for development', {exact: true}).check();
  await page.getByRole('button', {name: 'Connect', exact: true}).click();
  await page.locator('#workbench').waitFor({state: 'visible'});
}
// Register the fixture directory before first-use initialization can consult the OS Documents folder.
export async function registerFixtureWorkspace(request, service, registration = service.registration) {
  const headers={authorization:`Bearer ${registration.token}`,'x-rsi-wire-version':'2'};
  const describe=await request.post(service.origin+'/api/v2/rsi-api-protocol/5/connection/describe/1/read',{headers,data:{wire_version:2}});
  assert.equal(describe.status(),200);
  const {name,version}=JSON.parse(await readFile(new URL('../../../crates/rsi-workspace/api/wire-contract.json',import.meta.url),'utf8'));
  const coordinates={location:{kind:'local'},path:service.workspace};
  const registered=await request.post(`${service.origin}/api/v2/${name}/${version}/workspace/register/5/mutation`,{
    headers:{...headers,'x-rsi-host-epoch':describe.headers()['x-rsi-host-epoch']},data:coordinates,
  });
  assert.equal(registered.status(),200);
  assert.deepEqual((await registered.json()).coordinates,coordinates);
}
// Exercise the same cookie, CSRF and Host-generation checks as the real Worker.
export async function readBrowserCaller(page, expectedDevice) {
  return page.evaluate(async expected => {
    const root = '/api/v2/rsi-api-protocol/5/connection/';
    const headers = {'content-type':'application/json','x-rsi-csrf':'1','x-rsi-wire-version':'2'};
    const description = await fetch(root+'describe/1/read', {
      method:'POST',headers,body:JSON.stringify({wire_version:2}),
    });
    if (!description.ok) throw new Error('Connection description returned '+description.status);
    headers['x-rsi-host-epoch'] = description.headers.get('x-rsi-host-epoch');
    if (expected) headers['x-rsi-expected-device'] = expected;
    const reply = await fetch(root+'caller/1/read',{method:'POST',headers,body:'{}'});
    return {status:reply.status,device:reply.ok ? (await reply.json()).device_id : null};
  }, expectedDevice);
}
export async function openWorkspace(page, service) {
  await workspaceForm(page);
  await page.getByLabel('Directory path').fill(service.workspace);
  await page.locator('.directory-picker footer').getByRole('button',{name:'Open',exact:true}).click();
  await page.getByRole('button',{name:'New conversation in workspace',exact:true}).click();
  await detailMode(page,'verbose');
}

// Keep the starting surface unattached until the feature being exercised needs a Session.
export async function chooseStartingWorkspace(page, service) {
  await workspaceForm(page);
  const dialog = page.getByRole('dialog', {name:'Select workspace directory'});
  await dialog.getByLabel('Directory path').fill(service.workspace);
  await dialog.locator('footer').getByRole('button',{name:'Open',exact:true}).click();
  await dialog.waitFor({state:'hidden'});
  await page.locator('.pane.selected').getByRole('button',{name:/Conversation workspace,/}).click();
  await page.getByRole('menuitemradio',{name:'workspace',exact:true}).click();
}
