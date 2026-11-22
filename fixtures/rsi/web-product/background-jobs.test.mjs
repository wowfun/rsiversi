import assert from 'node:assert/strict';
import test from 'node:test';
import {chromium, firefox} from 'playwright';
import {sourceFixture} from './source-fixture.mjs';

test('session jobs observe visible output and separate human controls from model collection', async t => {
  const fixture = await sourceFixture({name:'session-background-jobs', source:`
    import React from 'react';
    import {createRoot} from 'react-dom/client';
    import {SessionPanel} from '/src/session/session-panel.tsx';
    import {IconSprite} from '/src/icon-sprite.tsx';
    import {installActions} from '/src/bridge.ts';
    const toolbar = document.createElement('header');
    document.body.prepend(toolbar);
    const root = createRoot(document.getElementById('root'));
    window.events = [];
    window.owner = true;
    const now = Date.now();
    const rows = Array.from({length:8}, (_,i) => ({
      id:'job-'+i, origin:'origin-'+i, name:'command '+i, status:i === 7 ? 'completed' : 'running',
      started_ms:now-10000+i*1000, finished_ms:i === 7 ? now : null, output_retained:true,
    }));
    window.show = (generation='1') => root.render(<><IconSprite/>
      <header className="pane-header"><div className="pane-heading">
        <SessionPanel titleActions={toolbar} chat local pane="primary" path="/work" generation={generation} jobs={[...rows]} />
      </div></header></>);
    installActions({call:async (method, payload) => {
      if (method !== 'background_job') throw new Error(method);
      const request = JSON.parse(payload);
      window.events.push(request);
      switch(request.action.kind) {
        case 'control': return JSON.stringify(window.owner);
        case 'peek': return JSON.stringify({stdout:'retained stdout',stderr:'\\nretained stderr',truncated:true});
        case 'stop': {
          const job = rows.find(row => row.id === request.action.job.id);
          if (!window.owner || job.origin !== request.action.job.origin) throw new Error('invalid stop');
          job.status = 'cancelled'; job.finished_ms = Date.now(); window.show();
          return 'null';
        }
      }
    }});
    window.show();
  `});
  t.after(() => fixture.close());
  for (const [name, engine] of Object.entries({chromium,firefox})) {
    await t.test(name, async t => {
      const browser = await engine.launch(); t.after(() => browser.close());
      const page = await browser.newPage({viewport:{width:390,height:844}});
      const errors = []; page.on('pageerror', error => errors.push(error.message));
      await page.goto(fixture.url);
      const toggle = page.getByRole('button',{name:/^Session panel/});
      await page.getByRole('button',{name:'Session panel Background jobs running: 7',exact:true}).waitFor();
      assert.deepEqual(await page.evaluate(() => window.events), []);
      await toggle.click();
      const panel = page.getByRole('region',{name:'Background jobs'});
      const row = label => panel.getByRole('button',{name:label,exact:true});
      await row('Stop task command 6').waitFor();
      assert.equal(await panel.locator('[data-job-label]').count(),5);
      assert.equal(await row('Finished 1').getAttribute('aria-expanded'),'false');
      await row('Show 2 more running (running background jobs)').click();
      await row('Show live output of command 0').click();
      await panel.getByText('retained stdout',{exact:true}).waitFor();
      await panel.getByText('retained stderr',{exact:true}).waitFor();
      await page.waitForFunction(() => window.events.filter(event => event.action.kind === 'peek').length >= 2);
      await row('Show less (running background jobs)').click();
      await panel.locator('[data-terminal]').waitFor({state:'detached'});
      const count = await page.evaluate(() => window.events.length);
      await page.waitForTimeout(550);
      assert.equal(await page.evaluate(() => window.events.length),count);
      await row('Stop task command 6').click();
      assert.equal(await page.evaluate(() => window.events.filter(event => event.action.kind === 'stop').length),0);
      await row('Click again to confirm').click();
      await row('Finished 2').click();
      await row('Show live output of command 6').click();
      await panel.getByText('retained stderr',{exact:true}).waitFor();
      await row('Clear').click();
      assert.equal(await row('Finished 2').count(),0);
      await toggle.click(); await toggle.click();
      assert.equal(await row('Finished 2').count(),0);
      await page.evaluate(() => { window.owner = false; window.show('2'); });
      await page.waitForFunction(() => window.events.some(event => event.generation === '2' && event.action.kind === 'control'));
      assert.equal(await panel.getByRole('button',{name:/^Stop task/}).count(),0);
      const stopped = await page.evaluate(() => window.events.filter(event => event.action.kind === 'stop'));
      assert.deepEqual(stopped,[{pane:'primary',generation:'1',action:{kind:'stop',job:{id:'job-6',origin:'origin-6'}}}]);
      assert.deepEqual(errors,[]);
    });
  }
});
