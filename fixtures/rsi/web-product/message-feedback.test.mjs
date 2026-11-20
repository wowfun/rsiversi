import assert from 'node:assert/strict';
import {test} from 'node:test';
import {chromium, firefox} from 'playwright';
import {sourceFixture} from './source-fixture.mjs';

test('answer feedback waits for its version and preserves a conflicted draft', async t => {
  const fixture = await sourceFixture({name:'message-feedback-cas', source:`
    import {createRoot} from 'react-dom/client';
    import {input} from '/src/bridge.ts';
    import {MessageFeedback} from '/src/session/message-feedback.tsx';

    window.calls = [];
    input.messageFeedback = (pane, generation, message, action) => {
      window.calls.push({pane, generation, message, action});
      if (action.kind === 'read') {
        return new Promise(resolve => {
          window.finishRead = () => resolve({current:{rating:'negative',version:'5'}});
        });
      }
      return Promise.resolve(action.version === '5'
        ? {conflict:true, current:{rating:'negative',version:'6'}}
        : {conflict:false, current:{rating:'positive',version:'7'}});
    };
    createRoot(document.getElementById('root')).render(
      <MessageFeedback pane="left" generation="8" message="11"/>
    );
  `});
  t.after(() => fixture.close());
  for (const [name, engine] of Object.entries({chromium, firefox})) {
    await t.test(name, async t => {
      const browser = await engine.launch();
      t.after(() => browser.close());
      const page = await browser.newPage();
      await page.goto(fixture.url);
      const like = page.getByRole('button', {name:'Good response', exact:true});
      const dialog = page.getByRole('dialog', {name:'Submit feedback'});
      await like.waitFor();
      assert.equal(await page.evaluate(() => window.calls.length), 0);
      await like.focus();
      await like.press('Enter');
      assert.equal(await like.getAttribute('aria-disabled'), 'true');
      await page.keyboard.press('Enter');
      assert.equal(await dialog.count(), 0);
      assert.deepEqual(await page.evaluate(() => window.calls), [
        {pane:'left', generation:'8', message:'11', action:{kind:'read'}},
      ]);
      await page.evaluate(() => window.finishRead());
      await dialog.waitFor();
      await page.keyboard.press('Escape');
      await dialog.waitFor({state:'detached'});
      assert(await like.evaluate(button => document.activeElement === button), 'Escape returns focus to the rating button');
      await like.press('Enter');
      await dialog.getByRole('textbox').fill('keep this note');
      const submit = dialog.getByRole('button', {name:'Submit', exact:true});
      await submit.click();
      await dialog.getByRole('alert').waitFor();
      assert.equal(await dialog.getByRole('textbox').inputValue(), 'keep this note');
      assert.equal(await page.evaluate(() => window.calls.length), 2);
      await submit.click();
      await dialog.waitFor({state:'detached'});
      assert.equal(await page.getByRole('button', {name:'Remove rating'}).getAttribute('aria-pressed'), 'true');
      assert.deepEqual(await page.evaluate(() => window.calls.slice(1)), ['5', '6'].map(version => ({
        pane:'left', generation:'8', message:'11',
        action:{kind:'change', version, value:{rating:'positive', note:'keep this note'}},
      })));
    });
  }
});
