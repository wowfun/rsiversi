// Current source uses the real document owner, without rewriting its imports or closure.
import {mountWorkbench} from './src/workbench-root.tsx';
import {DraftStore} from './drafts.js';
const {runtime}=mountWorkbench(document.getElementById('root'),{nativeDocument:false});
const fixtureKey='main', panes=runtime.panes;
let frameId=0;
let fixtureView;
let fixtureSamples = [];
let fixtureRun = 0;
let fixtureCount = 0;
let fixtureTextBytes = 0;
let fixtureInputStarted = 0;
function fixtureMarkdown(text) {return [{kind:'start',element:{kind:'paragraph'}},{kind:'text',text},{kind:'end'}]}
const fixtureModel = {deployment:'fixture',model:'deterministic'};
const fixtureMounts = {render:async()=>({}),close:async()=>{}};
const fixtureDrafts = await DraftStore.open('a'.repeat(32), fixtureKey === 'main' ? {kind:'local'} : 'b'.repeat(32));
const fixtureTransport = {
  terminate() {},
  postMessage(message) {
    const result=message.method==='connect'?JSON.stringify({endpoint_id:'a'.repeat(32),principal:{kind:'local'}}):message.method==='terminal'?JSON.stringify({value:[]}):'{}';
    queueMicrotask(() => this.onmessage({data:{kind:'reply',id:message.id,result}}));
  },
};
await runtime.attach(fixtureTransport,fixtureMounts,{},async()=>({drafts:fixtureDrafts}));
const connection=runtime.connection;
async function render(view) { await fixtureTransport.onmessage({data:{kind:'view',view:{kind:'snapshot',frame_id:String(++frameId),view},assets:{}}}); }
function fixturePane() { return fixtureKey === 'main' ? panes.get(fixtureKey) : panes[fixtureKey]; }
window.rsiPerformance = {
  get samples() {return fixtureSamples},
  async setup(count,run,bytesPerBlock=0) {
    fixtureCount=count; fixtureRun=run;
    const blocks=Array.from({length:count},(_,index)=>({key:`block-${index}`,role:'assistant',title:'Assistant',text:`## Result ${index}\n\nA bounded response with code, reasoning and a verification record.\n\n- Source checked\n- Result recorded\n\nlet answer = 42;`,clipped:false,sources:0,markdown:fixtureMarkdown(`Result ${index}: a bounded response with code and a verification record. Source checked; result recorded. let answer = 42;`)}));
    if(bytesPerBlock) for(const block of blocks) { block.text=block.text.padEnd(bytesPerBlock,'x'); block.markdown=fixtureMarkdown(`Result ${blocks.indexOf(block)}: ${block.text}`); }
    fixtureTextBytes=blocks.reduce((bytes,block)=>bytes+new TextEncoder().encode(block.text).length,0);
    const data={generation:String(count),selection:String(run+1),session:`performance-${count}`,header:'c'.repeat(64),creation:null,path:'/workspace/performance',draft:'',model:fixtureModel,transcript:{blocks,status:'Running',omitted:false},pending:[],notice:'',history_more:false,historical:false,ui_surfaces:[],block_actions:[],extension:{}};
    const workspace={id:'fixture',coordinates:{location:{kind:'local'},path:'/workspace/performance'}};
    fixtureView={
      notice:'',
      setup:{ticket:'fixture-setup',allowed:false,credentials_available:false,providers_writable:false,settings_writable:false,agent:{default_model:fixtureModel},presets:null,providers:null,diagnostic:null,receipts:[]},
      catalog:{workspaces:[workspace],workspaces_more:false,workspace_order_seed:{kind:'available',records:[workspace]},models:[fixtureModel],models_more:false},
      preferences:{submit_key:"mod_enter",busy_submit:"queue"},
      composer_actions:{main:{revision:'1',primary:{id:'queue',label:'Send'},alternative:{id:'steer',label:'Steer'}}},
      application_surfaces:[],has_remote_ui:false,
    };
    if(fixtureKey==='main') fixtureView.surfaces={main:data}; else fixtureView.panes=[data,null];
    await render(fixtureView); await fixturePane().binding;
    fixturePane().edit(''); await fixturePane().flush(); fixturePane().input.focus();
    const input=fixturePane().input;
    if(!input.dataset.perf) {
      input.dataset.perf='yes';
      input.addEventListener('input',()=>{fixtureInputStarted=performance.now()},{capture:true});
      input.addEventListener('input',async()=>{
        const started=fixtureInputStarted,count=fixtureCount,run=fixtureRun,text_bytes=fixtureTextBytes;
        const data=fixtureKey==='main'?fixtureView.surfaces.main:fixtureView.panes[0];
        data.transcript={...data.transcript,blocks:data.transcript.blocks.map((block,index)=>index===count-1?{...block,text:`Streaming update ${input.value}`,markdown:fixtureMarkdown(`Streaming update ${input.value}`)}:block)};
        await render({...fixtureView});
        requestAnimationFrame(()=>requestAnimationFrame(()=>fixtureSamples.push({blocks:count,run,text_bytes,input_to_paint_ms:performance.now()-started})));
      });
    }
    await new Promise(resolve=>requestAnimationFrame(()=>requestAnimationFrame(resolve)));
    return {text_bytes:fixtureTextBytes,blocks:fixturePane().transcript.querySelectorAll('.message').length,input:input.getBoundingClientRect().width,body:fixturePane().transcript.querySelector('.message-text')?.textContent};
  },
  async trajectory() {
    const data=fixtureKey==='main'?fixtureView.surfaces.main:fixtureView.panes[0];
    const roles=['assistant','reasoning','tool'];
    data.transcript={...data.transcript,blocks:data.transcript.blocks.map((block,index)=>({...block,key:`trajectory-${block.key}`,role:roles[index%3],title:roles[index%3],markdown:index%3===0?block.markdown:undefined}))};
    await render({...fixtureView});
    const detail=document.querySelector('[aria-label="Detail level"]');detail.value='verbose';detail.dispatchEvent(new Event('change',{bubbles:true}));
    await new Promise(resolve=>requestAnimationFrame(()=>requestAnimationFrame(resolve)));
    return {laidOut:[...fixturePane().transcript.querySelectorAll('.message')].filter(node=>node.getClientRects().length).length,reasoning:fixturePane().transcript.querySelectorAll('.reasoning').length,tools:fixturePane().transcript.querySelectorAll('.tool').length};
  },
  async close() { await fixturePane().flush(); await runtime.dispose(); }
};
