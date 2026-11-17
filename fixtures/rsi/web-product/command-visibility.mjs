import assert from 'node:assert/strict';
import {mkdir, writeFile} from 'node:fs/promises';
import {join, resolve} from 'node:path';
import {chromium, firefox} from 'playwright';
import {sourceFixture} from './source-fixture.mjs';

const report = resolve(process.env.RSI_REPORT_DIR);
await mkdir(report, {recursive:true});
const fixture = await sourceFixture({name:'command-visibility', source:`
import React,{useState} from 'react';
import {createRoot} from 'react-dom/client';
import {flushSync} from 'react-dom';
import {SessionCommands} from '/src/session/projections.tsx';
import surface from '/src/session/surface.module.css';
import '/styles.css';
const internal={name:'structured',description:'Dedicated control',draft_safe:false,composer_input:null};
const help={name:'help',description:'Show available help',draft_safe:true,composer_input:{kind:'none'}};
function App(){
 const [mode,setMode]=useState('structured');
 window.setCommands=value=>flushSync(()=>setMode(value));
 const commands=mode==='empty'?[]:mode==='mixed'?[internal,help]:[internal];
 const data={commands:{revision:{kind:'draft'},commands},command_receipt:mode==='receipt'?{command:'structured',request_id:'accepted-request',outcome:{kind:'committed',control_seq:'3'}}:null};
 const owner={commandsExpanded:mode!=='collapsed'&&mode!=='receipt',selectCommand(){}};
 return <main className={surface.scope} style={{padding:24}}><h1>Conversation commands</h1><SessionCommands data={data} owner={owner}/><p>Conversation content remains available.</p></main>;
}
createRoot(document.getElementById('root')).render(<App/>);
`});
const results=[];
try {
  for (const [name, engine] of Object.entries({chromium,firefox})) {
    const browser=await engine.launch();
    try {
      const page=await browser.newPage({viewport:{width:900,height:500}});
      const errors=[];
      page.on('pageerror',error=>errors.push(error.message));
      await page.goto(fixture.url);
      await page.getByRole('heading',{name:'Conversation commands'}).waitFor();
      const panel=page.locator('.session-commands');
      await page.screenshot({path:join(report,name+'-structured.png')});
      assert.equal(await panel.isHidden(),true,'structured-only catalogs have no empty panel');
      for (const mode of ['empty','collapsed']) {
        await page.evaluate(mode=>window.setCommands(mode),mode);
        assert.equal(await panel.isHidden(),true,mode);
      }
      await page.evaluate(()=>window.setCommands('mixed'));
      assert.equal(await panel.isVisible(),true);
      assert.deepEqual(await panel.getByRole('button').allTextContents(),['/help']);
      await page.screenshot({path:join(report,name+'-mixed.png')});
      await page.evaluate(()=>window.setCommands('receipt'));
      assert.equal(await panel.isVisible(),true,'receipt remains accessible with commands collapsed');
      assert.match(await panel.textContent(),/structured.*Committed/s);
      await page.screenshot({path:join(report,name+'-receipt.png')});
      assert.deepEqual(errors,[]);
      results.push({engine:name,version:browser.version(),structuredHidden:true,mixedButtons:1,receiptVisible:true,pageErrors:errors});
    } finally { await browser.close(); }
  }
  await writeFile(join(report,'results.json'),JSON.stringify(results,null,2));
} finally { await fixture.close(); }
