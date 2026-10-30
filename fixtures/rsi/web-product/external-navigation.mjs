import assert from 'node:assert/strict';
import {sourceFixture} from './source-fixture.mjs';

export async function verifyExternalNavigation(browser) {
  const fixture = await sourceFixture({name:'external-navigation-fixture',source:`
import React from 'react';
import {createRoot} from 'react-dom/client';
import {ExternalNavigation} from './src/navigation-external.tsx';
import {installActions,publish} from './src/bridge.ts';
const calls=[];let failed=false;
installActions({command:async value=>calls.push(value),open:async value=>{calls.push(value);if(value.action==='external_start'&&!failed){failed=true;throw Error('uncertain start')}}});
window.fixture={calls,catalog(value){publish({external_catalog:value})}};
fixture.catalog({endpoints:[],conversations:[],more:false});
createRoot(document.getElementById('root')).render(<ExternalNavigation/>);
`});
  const page = await browser.newPage(), errors = [];
  page.on('pageerror', error => errors.push(error.message));
  try {
    await page.goto(fixture.url);
    await page.waitForFunction(() => !!window.fixture);
    assert.equal(await page.locator('.external-navigation').count(), 0);
    await page.evaluate(() => fixture.catalog({endpoints:[{id:'fixture',enabled:true},{id:'disabled',enabled:false}],conversations:[{id:'retained',endpoint:'fixture',cwd:'/workspace/demo'}],more:true}));
    await page.locator('.external-navigation > summary').click();
    assert.equal(await page.getByRole('button',{name:'Start disabled',exact:true}).count(), 0);
    await page.getByRole('button',{name:'Start fixture',exact:true}).click();
    await page.getByRole('button',{name:'Check start fixture',exact:true}).click();
    await page.getByRole('button',{name:'Start fixture',exact:true}).waitFor();
    await page.locator('[title="retained"]').click();
    await page.getByRole('button',{name:'Refresh external conversations',exact:true}).click();
    await page.getByRole('button',{name:'More external conversations',exact:true}).click();
    const calls = await page.evaluate(() => fixture.calls);
    assert.equal(calls.length, 5);
    assert.deepEqual(calls[0], calls[1]);
    assert.match(calls[0].id, /^external_[0-9a-f-]{36}$/);
    assert.equal(calls[0].endpoint, 'fixture');
    assert.deepEqual(calls.slice(2), [{action:'external_open',id:'retained'},{action:'external_catalog',next:false},{action:'external_catalog',next:true}]);
    await page.evaluate(() => fixture.catalog({endpoints:[],conversations:[],more:false}));
    await page.locator('.external-navigation').waitFor({state:'detached'});
    assert.deepEqual(errors, []);
  } finally {await page.close(); await fixture.close();}
}
