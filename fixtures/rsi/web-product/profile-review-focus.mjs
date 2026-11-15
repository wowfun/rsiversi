import assert from 'node:assert/strict';
import {mkdir,writeFile} from 'node:fs/promises';
import {join,resolve} from 'node:path';
import {chromium,firefox} from 'playwright';
import {sourceFixture} from './source-fixture.mjs';
const report=resolve(process.env.RSI_REPORT_DIR);await mkdir(report,{recursive:true});
const scene=await sourceFixture({name:'profile-review-focus',source:`
import {createRoot} from 'react-dom/client';
import {useState} from 'react';
import {ProfileLeaves} from './src/profile-leaves.tsx';
import './styles.css';
import css from './src/settings/settings.module.css';
document.documentElement.dataset.theme='light';
const target={root:'root',profile:'host.toml',leaf:'tools'};
const preview={host_epoch:'epoch',target,operation:'disable',digest:'review',source_digest:'source',plugin:'fixture.tools',previous_enabled:true,enabled:false,effective_enabled:false};
const initial={available:true,can_grant:false,query:{profile:'host.toml',after:null},catalog:{principal:{kind:'local'},root:'root',profiles:['host.toml'],leaves:[{target,plugin:'fixture.tools',enabled:true,effective_enabled:true,allowed:['disable']}],next:null},grants:null,preview,result:null,notice:null};
function Scene(){const [view,setView]=useState(initial);window.fixture={reset(){setView({...initial})}};return <div className={css.scope} style={{padding:24}}><ProfileLeaves view={view} busy={false} command={value=>{if(value.kind==='commit')setView({...view,preview:null,result:{preview,outcome:{state:'saved',directory_synced:true,application:'applied'}}});if(value.kind==='discard')setView({...view,preview:null})}}/></div>}
createRoot(document.getElementById('root')).render(<Scene/>);
`});
const evidence=[];
try{for(const [name,engine] of Object.entries({chromium,firefox})){
  const browser=await engine.launch();
  try{
    const page=await browser.newPage({viewport:{width:1100,height:800}}),errors=[];
    page.on('pageerror',error=>errors.push(error.message));
    await page.goto(scene.url);
    const save=page.getByRole('button',{name:'Save reviewed change'});
    await save.focus();await page.keyboard.press('Enter');
    const result=page.getByRole('article',{name:'Source change result'});
    await result.waitFor();assert(await result.evaluate(node=>node===document.activeElement));
    assert.match(await result.innerText(),/disable.*tools/);
    await page.screenshot({path:join(report,`${name}-saved.png`)});
    await page.evaluate(()=>fixture.reset());
    await page.getByRole('button',{name:'Dismiss review'}).focus();await page.keyboard.press('Enter');
    await page.getByRole('article',{name:'Prepared tools'}).waitFor({state:'detached'});
    assert(await page.getByRole('combobox',{name:'Plugin leaf'}).evaluate(node=>node===document.activeElement));
    assert.deepEqual(errors,[]);evidence.push({browser:name,saveFocus:true,dismissFocus:true,resultIdentity:true});
  }finally{await browser.close()}
}}finally{await scene.close()}
await writeFile(join(report,'result.json'),JSON.stringify(evidence,null,2));console.log(JSON.stringify(evidence));
