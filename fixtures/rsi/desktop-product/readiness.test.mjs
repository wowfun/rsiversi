import assert from 'node:assert/strict';
import {readFile} from 'node:fs/promises';
import {createServer} from 'node:http';
import test from 'node:test';
import {chromium, firefox} from '../web-product/node_modules/playwright/index.mjs';
const focus = await readFile(new URL('./focus-input.js', import.meta.url), 'utf8');
const composerFocus = await readFile(new URL('./composer-focus.js', import.meta.url), 'utf8');
const assets = await readFile(new URL('./asset-pressure.js', import.meta.url), 'utf8');
const buttonTarget = await readFile(new URL('./button-target.js', import.meta.url), 'utf8');
const savedInput = await readFile(new URL('./saved-input.js', import.meta.url), 'utf8');
const drafts = await readFile(new URL('../../../apps/web/drafts.js', import.meta.url), 'utf8');
for (const [name, engine] of Object.entries({chromium, firefox})) {
  test(`${name}: reload waits for the exact committed Session draft`, async () => {
    const server = createServer((request, response) => {
      response.setHeader('Content-Type', request.url === '/drafts.js' ? 'text/javascript' : 'text/html');
      response.end(request.url === '/drafts.js' ? drafts : '<textarea aria-label="Main message">saved across document reload</textarea>');
    });
    await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
    let browser;
    try {
      browser = await engine.launch({headless: true});
      const page = await browser.newPage();
      await page.goto(`http://127.0.0.1:${server.address().port}`);
      assert.equal(await page.evaluate(source => new Function(source)('captured-session'), savedInput), true);
      await page.evaluate(async () => {
        const {DraftStore} = await import('/drafts.js');
        const store = await DraftStore.open('a'.repeat(32), {kind: 'device', device_id: 'b'.repeat(32)});
        let record = await store.ensure('main', 'captured-session', 'c'.repeat(64), null);
        record = await store.edit(record, 'sa', [], []);
        window.fixtureDraft = {store, record};
      });
      const check = (session = 'captured-session') => page.evaluate(({source, session}) => new Function(source)(session, 'saved across document reload'), {source: savedInput, session});
      assert.equal(await page.locator('textarea').inputValue(), 'saved across document reload');
      assert.equal(await check(), false, 'visible text and partial durable text do not establish a saved draft');
      await page.evaluate(async () => {
        const {DraftStore} = await import('/drafts.js');
        const other = await DraftStore.open('a'.repeat(32), {kind: 'local'});
        const record = await other.ensure('main', 'captured-session', 'c'.repeat(64), null);
        await other.edit(record, 'saved across document reload', [], []);
      });
      assert.equal(await check(), false, 'a full draft in the Host local namespace cannot satisfy the Device writer');
      await page.evaluate(async () => {const {store, record} = fixtureDraft;fixtureDraft.record = await store.edit(record, 'saved across document reload', [], []);});
      assert.equal(await check(), true);
      assert.equal(await check('another-session'), false, 'another Session cannot satisfy the captured draft');
    } finally {
      await browser?.close();
      await new Promise(resolve => server.close(resolve));
    }
  });
  test(`${name}: composer clear readiness excludes the hidden device receipt`, async () => {
    const browser = await engine.launch({headless: true});
    try {
      const page = await browser.newPage();
      await page.setContent('<form hidden><textarea id="receipt"></textarea></form><textarea aria-label="Main message">/plan on</textarea>');
      assert.equal(await page.evaluate(() => document.querySelector('textarea').value === ''), true, 'the generic selector falsely reports readiness');
      const cleared = () => page.evaluate(source => new Function(source)('textarea[aria-label="Main message"]', 'value') === '', focus);
      assert.equal(await cleared(), false, 'an empty receipt does not finish clearing the command');
      await page.getByRole('textbox', {name: 'Main message'}).evaluate(input => {input.value = '';});
      assert.equal(await cleared(), true);
      await page.getByRole('textbox', {name: 'Main message'}).evaluate(input => input.remove());
      assert.equal(await cleared(), false, 'an absent composer is not ready for the next prompt');
    } finally {await browser.close();}
  });
  test(`${name}: native button selection excludes hidden duplicates and covered controls`, async () => {
    const browser=await engine.launch({headless:true});
    try {
      const page=await browser.newPage();
      await page.setContent('<details><summary>Menu</summary><button>Workspace files</button></details><fieldset disabled><button>Workspace files</button></fieldset><div inert><button>Workspace files</button></div><button id="visible">Workspace files</button>');
      const check=()=>page.evaluate(source=>new Function(source)('Workspace files')?.id??null,buttonTarget);
      assert.equal(await check(),'visible');
      await page.evaluate(()=>{const cover=document.createElement('div');cover.style='position:fixed;inset:0;z-index:999';document.body.append(cover)});
      assert.equal(await check(),null);
    } finally {await browser.close();}
  });
  test(`${name}: repeated queue labels remain scoped to their original slot`, async () => {
    const browser=await engine.launch({headless:true});
    try {
      const page=await browser.newPage();
      await page.setContent('<div class="queue-row" data-slot="other"><button id="other">Edit</button></div><div class="queue-row" data-slot="original"><button id="original">Edit</button></div>');
      const check=()=>page.evaluate(source=>new Function(source)('Edit','.queue-row[data-slot="original"]')?.id??null,buttonTarget);
      assert.equal(await check(),'original');
      await page.locator('[data-slot="original"]').evaluate(row=>row.remove());
      assert.equal(await check(),null,'another slot never substitutes for a missing original');
    } finally {await browser.close();}
  });
  test(`${name}: composer evidence rejects an earlier same-length trusted input`, async () => {
    const browser=await engine.launch({headless:true});
    try {
      const page=await browser.newPage();
      await page.setContent('<textarea aria-label="Main message" style="outline:2px solid blue"></textarea>');
      await page.evaluate(()=>{
        window.fixtureInputEvents=[];
        document.addEventListener('input',event=>window.fixtureInputEvents.push({length:event.target.value.length,trusted:event.isTrusted}));
      });
      const check=after=>page.evaluate(({source,after})=>new Function(source)(after),{source:composerFocus,after});
      await page.getByRole('textbox').focus();await page.keyboard.type('same');
      assert.equal((await check(0)).trustedInput,true);
      const before=await page.evaluate(()=>window.fixtureInputEvents.length);
      await page.getByRole('textbox').evaluate(input=>{input.value='next';});
      assert.equal((await check(before)).trustedInput,false,'an earlier event cannot prove the current fill');
      await page.getByRole('textbox').evaluate(input=>input.dispatchEvent(new Event('input',{bubbles:true})));
      assert.equal((await check(before)).trustedInput,false,'a new untrusted event is insufficient');
      await page.keyboard.press('ControlOrMeta+A');await page.keyboard.press('Backspace');await page.keyboard.type('next');
      const current=await check(before);
      assert.equal(current.trustedInput,true);assert.equal(current.focused,true);
      assert.equal(current.focusVisible,true);assert.equal(current.outlineWidth,2);
    } finally {await browser.close();}
  });
  test(`${name}: only the focused current input admits native typing`, async () => {
    const browser = await engine.launch({headless: true});
    try {
      const page = await browser.newPage({viewport: {width: 390, height: 400}});
      await page.setContent('<div style="height:1000px"></div><fieldset disabled><label>Deployment name<input aria-label="Deployment name"></label></fieldset>');
      const check = () => page.evaluate(({source}) => new Function(source).call(null, '[aria-label="Deployment name"]')?.getAttribute('aria-label') ?? null, {source: focus});
      assert.equal(await check(), null);
      await page.locator('fieldset').evaluate(e => {e.disabled = false; e.inert = true;});
      assert.equal(await check(), null);
      await page.locator('fieldset').evaluate(e => {e.inert = false;});
      assert.equal(await check(), 'Deployment name');
      assert.equal(await page.evaluate(() => document.activeElement.getAttribute('aria-label')), 'Deployment name');
      await page.keyboard.type('desktop-provider');
      assert.equal(await page.locator('input').inputValue(), 'desktop-provider');
      await page.locator('input').evaluate(e => {e.readOnly = true;});
      assert.equal(await check(), null);
    } finally {await browser.close();}
  });
  test(`${name}: loaded assets wait through a read polling gap without replay`, async () => {
    const browser = await engine.launch({headless: true});
    try {
      const page = await browser.newPage();
      await page.evaluate(() => {window.fixtureAssetPressure = {status: 200, module: '/renderer.js', pending: 0}; window.fixturePressure = {pending: 0};});
      const check = () => page.evaluate(source => new Function(source)(), assets);
      assert.equal(await check(), null);
      await page.evaluate(() => {window.fixturePressure.pending = 31;});
      assert.deepEqual(await check(), {status: 200, module: '/renderer.js', pendingAtCompletion: 0, pending: 31});
      await page.evaluate(() => {window.fixturePressure.error = 'read rejected';});
      await assert.rejects(check, /read rejected/);
      await page.evaluate(() => {delete window.fixturePressure.error; window.fixtureAssetPressure = {error: 'asset rejected'};});
      await assert.rejects(check, /asset rejected/);
    } finally {await browser.close();}
  });
}
