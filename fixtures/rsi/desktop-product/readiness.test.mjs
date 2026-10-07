import assert from 'node:assert/strict';
import {readFile} from 'node:fs/promises';
import test from 'node:test';
import {chromium, firefox} from '../web-product/node_modules/playwright/index.mjs';
const focus = await readFile(new URL('./focus-input.js', import.meta.url), 'utf8');
const assets = await readFile(new URL('./asset-pressure.js', import.meta.url), 'utf8');
for (const [name, engine] of Object.entries({chromium, firefox})) {
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
