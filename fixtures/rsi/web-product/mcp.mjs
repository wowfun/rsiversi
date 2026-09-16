import assert from 'node:assert/strict';
import {join} from 'node:path';
import {writeFile} from 'node:fs/promises';
import {startMcpFixture} from './mcp-fixture.mjs';
export async function verifyMcp(page,pane,report,browser) {
  const fixture=await startMcpFixture();
  const draft=pane.getByRole('textbox',{name:'Main message',exact:true});
  try {
    await draft.fill('Retained while configuring MCP');
    await page.getByRole('button',{name:'Settings',exact:true}).click();await page.getByRole('button',{name:'Plugins',exact:true}).click();
    const panel=page.getByRole('region',{name:'MCP connections',exact:true});await panel.waitFor();
    const edit=async config=>{
      await panel.getByRole('button',{name:'Edit HTTP endpoints',exact:true}).click();
      await page.getByRole('button',{name:'Edit as JSON',exact:true}).click();
      const editor=page.getByRole('textbox',{name:'Settings JSON',exact:true});await editor.fill(JSON.stringify(config));const old=await editor.elementHandle();
      await page.getByRole('button',{name:'Save settings',exact:true}).click();await page.waitForFunction(element=>!element.isConnected,old);await old.dispose();
      await page.getByRole('button',{name:'Close details',exact:true}).click();
    };
    await edit({servers:[{id:'fixture',enabled:true,tools:['echo'],transport:{kind:'streamable_http',url:fixture.url,credential:{owner:'rsi.mcp',slot:'browser-fixture'}}}]});
    await panel.getByRole('button',{name:'Read MCP status',exact:true}).click();await panel.getByText('Saved HTTP settings need to be applied.',{exact:false}).waitFor();
    await panel.getByRole('button',{name:'Apply and refresh HTTP',exact:true}).click();await panel.locator('.mcp-notice').filter({hasText:'credential is unavailable'}).waitFor();assert.equal(fixture.evidence.requests,0);
    const row=panel.locator('article').filter({has:page.getByRole('heading',{name:'fixture HTTP',exact:false})});
    await row.locator('.mcp-credential summary').click();await row.getByRole('button',{name:'Read credential status for fixture',exact:true}).click();
    const key=row.getByLabel('API key for fixture',{exact:true});await key.waitFor();await key.fill('isolated-mcp-fixture-secret');
    await row.getByRole('button',{name:'Save credential for fixture',exact:true}).click();await panel.locator('.mcp-notice').filter({hasText:'Credential saved'}).waitFor();assert.equal(await key.inputValue(),'');assert.equal(fixture.evidence.requests,0,'credential writes must not reconnect implicitly');
    await row.getByRole('button',{name:'Refresh fixture',exact:true}).click();await panel.getByText('MCP catalog verified.',{exact:false}).waitFor();
    await row.locator('details').filter({has:page.locator('summary').filter({hasText:'Verified tools'})}).locator('summary').click();
    assert.match(await row.innerText(),/echo · selected/);
    assert.deepEqual(fixture.evidence.methods,['server/discover','tools/list','resources/list']);
    assert.equal(fixture.evidence.authorized,fixture.evidence.requests);
    assert.equal(fixture.evidence.initializations,0);assert.equal(fixture.evidence.calls,0);
    const digest=await row.getByText('Last verified catalog',{exact:false}).innerText();fixture.evidence.revision=2;
    await row.getByRole('button',{name:'Refresh fixture',exact:true}).click();await page.waitForFunction(previous=>[...document.querySelectorAll('.mcp-panel .hint')].some(element=>element.textContent.startsWith('Last verified catalog')&&element.textContent!==previous),digest);
    await page.screenshot({path:join(report,`${browser}-mcp.png`)});await page.setViewportSize({width:420,height:860});await page.screenshot({path:join(report,`${browser}-mcp-narrow.png`)});assert(await panel.evaluate(element=>element.scrollWidth<=element.clientWidth+1));await page.setViewportSize({width:1440,height:980});
    assert(!await panel.innerText().then(text=>text.includes('isolated-mcp-fixture-secret')||text.includes(fixture.url)));
    assert(!await page.locator('#notice').innerText().then(text=>text.includes('MCP credential is unavailable')),'resolved MCP failure must not remain a global banner');
    await row.getByRole('button',{name:'Read credential status for fixture',exact:true}).click();await row.getByRole('button',{name:'Remove credential for fixture',exact:true}).click();await panel.locator('.mcp-notice').filter({hasText:'Credential removed'}).waitFor();
    await row.getByRole('button',{name:'Refresh fixture',exact:true}).click();await panel.locator('.mcp-notice').filter({hasText:'credential is unavailable'}).waitFor();
    await edit({servers:[]});await panel.getByRole('button',{name:'Apply and refresh HTTP',exact:true}).click();await panel.getByText('No MCP endpoints configured.',{exact:true}).waitFor();
    await page.getByRole('button',{name:'Close settings',exact:true}).click();assert.equal(await draft.inputValue(),'Retained while configuring MCP');await draft.fill('');
    await writeFile(join(report,`${browser}-mcp.json`),JSON.stringify({status:'passed',...fixture.evidence}));
  } finally {await fixture.close()}
}
