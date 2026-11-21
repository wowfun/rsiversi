import assert from 'node:assert/strict';
import test from 'node:test';
import {chromium, firefox} from 'playwright';
import {sourceFixture} from './source-fixture.mjs';

test('the conversation to-do disclosure follows replacement projections', async t => {
  const fixture = await sourceFixture({name:'todo-panel-fixture', source:`
    import React from 'react';
    import {createRoot} from 'react-dom/client';
    import {TodoPanel} from '/src/session/todo-panel.tsx';
    import {IconSprite} from '/src/icon-sprite.tsx';
    import surfaceCss from '/src/session/surface.module.css';
    const root = createRoot(document.querySelector('#root'));
    window.show = (value, session = 'first', takeover = false) => root.render(
      <><IconSprite/>{takeover && <div className={surfaceCss.approval}>Review</div>}
        <TodoPanel key={session} projections={{entries:[{producer:'rsi.todo.view',content:{kind:'value',value}}]}}/>
      </>
    );
    window.show([]);
  `});
  t.after(() => fixture.close());
  for (const [name, engine] of Object.entries({chromium, firefox})) {
    await t.test(name, async t => {
      const browser = await engine.launch();
      t.after(() => browser.close());
      const page = await browser.newPage();
      await page.goto(fixture.url);
      await page.waitForFunction(() => typeof window.show === 'function');
      const panel = page.getByRole('region', {name:'To-dos'});
      assert.equal(await panel.count(), 0);
      const items = [
        {content:'Inspect', status:'completed'},
        {content:'Verify', status:'in_progress'},
        {content:'Verify', status:'pending'},
      ];
      await page.evaluate(items => window.show(items), items);
      const header = panel.getByRole('button');
      await header.waitFor();
      assert.match(await header.innerText(), /1 completed.*1 in progress.*1 pending/s);
      assert.equal(await header.getAttribute('aria-expanded'), 'false');
      assert(await header.locator('use').evaluateAll(uses => uses.length === 2 &&
        uses.every(use => document.getElementById(use.getAttribute('href').slice(1)))));
      await header.press('Enter');
      await panel.getByRole('img', {name:'In progress'}).waitFor();
      assert.deepEqual(await panel.getByRole('listitem').allTextContents(), items.map(item => item.content));
      await page.evaluate(items => window.show(items, 'first', true), items);
      await panel.waitFor({state:'hidden'});
      await page.evaluate(items => window.show(items), items);
      await panel.getByRole('listitem').first().waitFor();
      await page.evaluate(() => window.show([{content:'Done',status:'completed'}]));
      await panel.getByRole('listitem').getByText('Done').waitFor();
      assert.equal(await header.getAttribute('aria-expanded'), 'true');
      assert.doesNotMatch(await header.innerText(), /pending|in progress/);
      await page.evaluate(items => window.show(items, 'another'), items);
      await panel.getByRole('button', {name:/in progress/}).waitFor();
      assert.equal(await header.getAttribute('aria-expanded'), 'false');
      await page.evaluate(() => window.show([], 'another'));
      await panel.waitFor({state:'detached'});
      await page.evaluate(() => window.show([{content:'Bad', status:'unknown'}]));
      await page.evaluate(() => new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(resolve))));
      assert.equal(await panel.count(), 0);
    });
  }
});
