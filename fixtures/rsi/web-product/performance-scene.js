// Inserted into an isolated copy of the document's lexical scope, never production.
const fixtureKey = typeof panes.get === 'function' ? 'main' : 0;
let fixtureView;
let fixtureSamples = [];
let fixtureRun = 0;
let fixtureCount = 0;
let fixtureTextBytes = 0;
let fixtureInputStarted = 0;
function fixtureMarkdown(text) {return [{kind:'start',element:{kind:'paragraph'}},{kind:'text',text},{kind:'end'}]}
const fixtureModel = {deployment:'fixture',model:'deterministic'};
mounts = await MountTable.open();
const fixtureDrafts = await DraftStore.open('a'.repeat(32), fixtureKey === 'main' ? {kind:'local'} : 'b'.repeat(32));
connection = {drafts:fixtureDrafts,mounts,pending:new Map(),worker:{terminate(){}},closing:false};
call = async method => {
  if (method === 'disconnect' && location.protocol === 'rsi:') return (await fetch('/_disconnect',{method:'POST',body:''})).json();
  return '{}';
};
connected = true;
$('login').hidden = true; $('workbench').hidden = false; $('sign-out').hidden = false;
$('connection-state').textContent = 'Deterministic performance fixture';
function fixturePane() { return fixtureKey === 'main' ? panes.get(fixtureKey) : panes[fixtureKey]; }
window.rsiPerformance = {
  get samples() {return fixtureSamples},
  async setup(count,run,bytesPerBlock=0) {
    fixtureCount=count; fixtureRun=run;
    const blocks=Array.from({length:count},(_,index)=>({key:`block-${index}`,role:'assistant',title:'Assistant',text:`## Result ${index}\n\nA bounded response with code, reasoning and a verification record.\n\n- Source checked\n- Result recorded\n\nlet answer = 42;`,clipped:false,sources:0,markdown:fixtureMarkdown(`Result ${index}: a bounded response with code and a verification record. Source checked; result recorded. let answer = 42;`)}));
    if(bytesPerBlock) for(const block of blocks) { block.text=block.text.padEnd(bytesPerBlock,'x'); block.markdown=fixtureMarkdown(`Result ${blocks.indexOf(block)}: ${block.text}`); }
    fixtureTextBytes=blocks.reduce((bytes,block)=>bytes+new TextEncoder().encode(block.text).length,0);
    const data={generation:String(count),selection:String(run+1),session:`performance-${count}`,header:'c'.repeat(64),creation:null,path:'/workspace/performance',draft:'',model:fixtureModel,transcript:{blocks,status:'Running',omitted:false},pending:[],notice:'',history_more:false,historical:false,ui_surfaces:[],block_actions:[],extension:{}};
    fixtureView={notice:'',setup:{agent:{default_model:fixtureModel}},catalog:{workspaces:[{id:'fixture',path:'/workspace/performance'}],sessions:[],models:[fixtureModel]},preferences:{enter_submit:false},application_surfaces:[],has_remote_ui:false};
    if(fixtureKey==='main') fixtureView.surfaces={main:data}; else fixtureView.panes=[data,null];
    render(fixtureView); await fixturePane().binding;
    fixturePane().edit(''); await fixturePane().flush(); fixturePane().input.focus();
    const input=fixturePane().input;
    if(!input.dataset.perf) {
      input.dataset.perf='yes';
      input.addEventListener('input',()=>{fixtureInputStarted=performance.now()},{capture:true});
      input.addEventListener('input',()=>{
        const started=fixtureInputStarted,count=fixtureCount,run=fixtureRun,text_bytes=fixtureTextBytes;
        const data=fixtureKey==='main'?fixtureView.surfaces.main:fixtureView.panes[0];
        data.transcript={...data.transcript,blocks:data.transcript.blocks.map((block,index)=>index===count-1?{...block,text:`Streaming update ${input.value}`,markdown:fixtureMarkdown(`Streaming update ${input.value}`)}:block)};
        render({...fixtureView});
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
    render({...fixtureView});
    [...document.querySelectorAll('button')].find(button=>button.textContent==='Trajectory')?.click();
    await new Promise(resolve=>requestAnimationFrame(()=>requestAnimationFrame(resolve)));
    return {laidOut:[...fixturePane().transcript.querySelectorAll('.message')].filter(node=>node.getClientRects().length).length,reasoning:fixturePane().transcript.querySelectorAll('.reasoning').length,tools:fixturePane().transcript.querySelectorAll('.tool').length};
  },
  async close() { await fixturePane().flush(); await mounts.close(); return call('disconnect'); }
};
