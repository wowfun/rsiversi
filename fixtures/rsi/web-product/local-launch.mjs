import './paired-env.mjs';
import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import http from 'node:http';
import { mkdtemp, mkdir, cp, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { chromium, firefox } from 'playwright';
import { boundedRun, deadline } from './service.mjs';

// Real built product, static bundle, Worker and cookies. No live provider or user state.
export async function verifyLocalLaunch(binary, assets, engines = [['chromium', chromium], ['firefox', firefox]]) {
  assert(engines.length > 0, 'Select at least one browser');
  const results = [];
  for (const [name, engine] of engines) {
    const root = await mkdtemp(join(tmpdir(), 'rlw-'));
    const executable = join(root, 'rsi');
    await cp(binary, executable);
    const env = { PATH: process.env.PATH };
    for (const [key, leaf] of [['HOME','home'],['XDG_CONFIG_HOME','config'],['XDG_STATE_HOME','state'],['XDG_CACHE_HOME','cache'],['XDG_RUNTIME_DIR','run']]) {
      env[key] = join(root, leaf); await mkdir(env[key], {mode:0o700});
    }
    let child, browser;
    let providerCalls = 0;
    const provider = http.createServer((_, response) => { providerCalls++; response.writeHead(500).end(); });
    await new Promise(resolve => provider.listen(0, '127.0.0.1', resolve));
    const run = args => boundedRun(executable, args, {env, cwd:root});
    async function stop() {
      if (!child || child.exitCode !== null) return;
      const exited = new Promise(resolve => child.once('exit', resolve));
      child.kill('SIGINT');
      await deadline(exited, 'local Web shutdown', 20_000);
      assert.equal(child.exitCode, 0);
    }
    async function start() {
      child = spawn(executable, ['web','--port','0','--no-open','--assets',assets], {env, cwd:root, stdio:['ignore','pipe','pipe']});
      return deadline(new Promise((resolve, reject) => {
        let output = '', errors = '';
        child.stderr.on('data', chunk => {errors = (errors + chunk).slice(-16_384)});
        child.stdout.on('data', chunk => {
          output = (output + chunk).slice(-2048);
          const match = /rsi web: (http:\/\/127\.0\.0\.1:\d+#rsi-launch=[a-f0-9]{64})/.exec(output);
          if (match) { output = ''; resolve(match[1]); }
        });
        child.once('exit', code => reject(new Error(`local Web exited (${code}): ${errors}`)));
      }), 'local Web readiness', 30_000);
    }
    try {
      const link = await start();
      browser = await engine.launch({headless:true});
      const context = await browser.newContext();
      const page = await context.newPage();
      const errors = [];
      page.on('pageerror', error => errors.push(error.message));
      const connected = async page => {
        await page.locator('#workbench').waitFor({state:'visible',timeout:30_000});
        assert.equal(new URL(page.url()).hash, '');
        assert.equal(await page.locator('#login').isVisible(), false);
      };
      const identity = page => page.evaluate(async () => {
        const response = await fetch('/api/v1/browser-bootstrap', {method:'POST',headers:{'X-Rsi-Csrf':'1'}});
        if (!response.ok) throw new Error('cookie recovery rejected');
        return response.json();
      });
      await page.goto(link);
      await connected(page);
      const first = await identity(page);
      await page.locator('#settings-open').click();
      await page.getByLabel('API key', {exact:true}).waitFor({state:'visible'});
      assert.equal(await page.getByLabel('API key', {exact:true}).isEnabled(), true);
      await page.getByLabel('API key', {exact:true}).fill('isolated-fixture-secret');
      await page.getByRole('button', {name:'Save credential',exact:true}).click();
      await page.waitForFunction(() => document.querySelector('.setup-receipts [data-outcome=confirmed]')?.textContent.includes('credential-set'));
      assert.equal(await page.locator('.permission-note').count(), 0);
      await page.getByLabel('Provider',{exact:true}).selectOption('openai-compatible');
      await page.getByLabel('Deployment name').fill('local-fixture');
      await page.getByLabel('Provider endpoint').fill(`http://127.0.0.1:${provider.address().port}`);
      await page.getByLabel('Request path').fill('/v1/chat/completions');
      await page.getByLabel('Model identifier 1',{exact:true}).fill('fixture-model');
      await page.getByRole('button',{name:'Apply provider',exact:true}).click();
      await page.getByText('Desired 1 · Applied 1',{exact:true}).waitFor();
      await page.getByLabel('Default model',{exact:true}).selectOption({label:'fixture-model · local-fixture'});
      await page.getByText('default_model · confirmed',{exact:true}).waitFor();
      await page.reload();
      await connected(page);
      assert.deepEqual(await identity(page), first);
      const tab = await context.newPage();
      await tab.goto(new URL(link).origin);
      await connected(tab);
      assert.deepEqual(await identity(tab), first);
      // A valid existing cookie must not hide a definitive ticket rejection.
      let rejectedExchanges = 0, rejectedRecoveries = 0;
      await context.route('**/api/v1/browser-bootstrap', async route => {
        if (route.request().headers()['x-rsi-launch-ticket']) rejectedExchanges++;
        else rejectedRecoveries++;
        await route.continue();
      });
      const stale = await context.newPage();
      await stale.goto(link);
      await stale.waitForFunction(() => document.body.textContent.includes('Restart rsi web'));
      assert.equal(await stale.locator('#workbench').isVisible(), false);
      assert.equal(rejectedExchanges, 1);
      assert.equal(rejectedRecoveries, 0);
      await stale.close();
      await context.unroute('**/api/v1/browser-bootstrap');
      for (const [status, code, message] of [
        [429, 'capacity', 'Local service is busy'],
        [409, 'generation_retired', 'Local service is shutting down'],
      ]) {
        let requests = 0;
        await context.route('**/api/v1/browser-bootstrap', async route => {
          requests++;
          await route.fulfill({status, contentType:'application/json', body:JSON.stringify({code})});
        });
        const rejected = await context.newPage();
        await rejected.goto(link);
        await rejected.waitForFunction(message => document.body.textContent.includes(message), message);
        assert.equal(await rejected.locator('#workbench').isVisible(), false);
        assert.equal(requests, 1, 'a classified rejection must not attempt cookie recovery');
        await rejected.close();
        await context.unroute('**/api/v1/browser-bootstrap');
      }
      // A consumed ticket cannot create a second authenticated browser.
      const stranger = await browser.newContext();
      const denied = await stranger.newPage();
      await denied.goto(link);
      await denied.waitForFunction(() => document.body.textContent.includes('Restart rsi web'));
      assert.equal(await denied.locator('#workbench').isVisible(), false);
      const receipt = JSON.parse(run(['--profile','devices','register','manual-browser']).stdout);
      await denied.getByLabel('Device registration receipt').fill(JSON.stringify(receipt));
      await denied.locator('#dev-http').check();
      await denied.getByRole('button',{name:'Connect',exact:true}).click();
      await connected(denied);
      await stranger.close();
      await page.getByRole('button',{name:'Settings',exact:true}).click();
      await page.getByRole('button',{name:'Plugins',exact:true}).click();
      const panel = page.getByRole('region',{name:'Plugins',exact:true});
      await panel.locator('.plugin-rows .plugin-row').first().waitFor();
      const grants = JSON.parse(run(['--profile','devices','configuration','list']).stdout);
      run(['--profile','devices','configuration','revoke',first.device_id,grants.revision]);
      await panel.getByRole('button',{name:'Refresh plugin status',exact:true}).click();
      await panel.getByRole('alert').filter({hasText:'Plugin status unavailable'}).waitFor();
      assert(!JSON.parse(run(['--profile','devices','configuration','list']).stdout).devices.includes(first.device_id));
      await tab.close();
      await page.close();
      await stop();
      const restarted = await start();
      const reopened = await context.newPage();
      let exchanges = 0, recoveries = 0;
      await context.route('**/api/v1/browser-bootstrap', async route => {
        if (route.request().headers()['x-rsi-launch-ticket']) {
          exchanges++;
          const response = await route.fetch();
          assert.equal(response.status(), 200);
          // Keep Set-Cookie but lose the identity result: never replay the ticket.
          await route.fulfill({response, body:'{'});
        } else {
          recoveries++;
          await route.continue();
        }
      });
      await reopened.goto(restarted);
      await connected(reopened);
      assert.equal(exchanges, 1);
      assert.equal(recoveries, 1);
      await context.unroute('**/api/v1/browser-bootstrap');
      assert.deepEqual(await identity(reopened), first);
      assert(JSON.parse(run(['--profile','devices','configuration','list']).stdout).devices.includes(first.device_id));
      assert.equal(providerCalls, 0, 'configuration must not invoke a provider');
      assert.deepEqual(errors, []);
      await context.close();
      results.push({browser:name, automaticLogin:true, configurationWrite:true, cookieRecovery:true, stableDevice:true, uncertainExchangeRecovery:true, definitiveRejectionPreserved:true, manualLogin:true, grantRevocation:true});
    } catch (error) {
      // A navigation failure can include its URL in Playwright's diagnostic.
      throw new Error(String(error.stack ?? error).replace(/rsi-launch=[a-f0-9]{64}/g, 'rsi-launch=[redacted]'));
    } finally {
      await new Promise(resolve => provider.close(resolve));
      await browser?.close();
      try { await stop(); } finally { if (child?.exitCode === null) child.kill('SIGKILL'); await rm(root,{recursive:true,force:true}); }
    }
  }
  return results;
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  const binary = process.env.RSI_WEB_BINARY, assets = process.env.RSI_WEB_ASSETS;
  assert.ok(binary && assets, 'Set RSI_WEB_BINARY and RSI_WEB_ASSETS to built artifacts');
  const engines = [['chromium',chromium],['firefox',firefox]].filter(([name]) => !process.env.RSI_WEB_BROWSER || process.env.RSI_WEB_BROWSER === name);
  console.log(JSON.stringify(await verifyLocalLaunch(binary, assets, engines)));
}
