import {test} from 'node:test';
import assert from 'node:assert/strict';
import {commandSelection,claimedCommand} from '../command-input.js';
import {createPaneClass} from '../src/runtime/session-controller.js';
import {pickCompletion} from '../src/runtime/session-completions.js';
import 'fake-indexeddb/auto';
import {DraftStore,DraftEditor} from '../drafts.js';
import {clearDraftRecords} from './clear-draft-records.mjs';

const plan={name:'plan',composer_input:{kind:'text',hint:'on | off | toggle'}};
test('skill completion places the caret before arguments and only changes an admitted draft',()=>{
  const item={group:'skill',replacement:'/audit-proof'};
  for(const tail of ['', ' prior text', '\tprior text']){
    const token={start:0,end:3,cursor:3,text:'/au'+tail};
    const input={value:token.text,setSelectionRange(start,end){this.selectionStart=start;this.selectionEnd=end;},focus(){}};
    const pane={input,generation:'one',completionState:{...token,generation:'one'},completionToken:()=>token,
      closeCompletions(){},edit(){throw new Error('Draft capacity');}};
    assert.throws(()=>pickCompletion(pane,{},item),/Draft capacity/);
    assert.equal(input.value,token.text);
    let saved;
    pane.edit=text=>{saved=text;};
    pickCompletion(pane,{},item);
    assert.equal(saved,item.replacement+(tail||' '));
    assert.equal(input.value,saved);
    assert.equal(input.selectionStart, '/audit-proof '.length);
    assert.equal(input.selectionEnd, input.selectionStart);
  }
});
test('rejected command insertion preserves displayed text and command state at both draft limits',async()=>{
  const store=await DraftStore.open('e'.repeat(32),{kind:'local'});
  try {
    for (const aggregate of [false,true]) {
      const record=await store.ensure('main',`limit-${aggregate}`,'a'.repeat(64));
      const editor=new DraftEditor(store,record);
      const text='x'.repeat(aggregate ? 512*1024 : 1024*1024);
      editor.edit(text);await editor.flush();
      const pane=new(createPaneClass({}))('main');
      Object.assign(pane,{generation:'one',editor,input:{value:text,selectionStart:0,selectionEnd:0,focus(){},setSelectionRange(){}},closeCompletions(){}});
      pane.editors.set('current',editor);
      if(aggregate)pane.editors.set('other',{textBytes:2*1024*1024-editor.textBytes});
      await assert.rejects(pane.selectCommand(plan),/limit|large|exceed/i);
      assert.equal(pane.input.value.length,text.length);
      assert.equal(pane.input.value,text);
      assert.equal(editor.text,text);assert.equal(editor.dirty,false);
      assert.equal(pane.selectedCommand,undefined);
    }
  } finally {await clearDraftRecords();}
});
test('command selection reports an unavailable editor separately from a stale selection',async()=>{
  const pane=new(createPaneClass({}))('main');
  pane.input={value:'',selectionStart:0,selectionEnd:0};
  await assert.rejects(pane.selectCommand(plan),/draft is not ready/i);
});
test('leaving command input preserves disabled and composing editors',()=>{
  const pane=new(createPaneClass({}))('main');
  const editor={text:'/plan keep this'};
  Object.assign(pane,{editor,composerView:{disabled:true,command:{name:'plan',end:6}},edit(text){editor.text=text;},focusInput(){}});
  pane.clearCommand();assert.equal(editor.text,'/plan keep this');
  pane.composerView.disabled=false;pane.composing=true;
  pane.clearCommand();assert.equal(editor.text,'/plan keep this');
  pane.composing=false;pane.clearCommand();assert.equal(editor.text,'keep this');
});

test('restored commands prefetch their catalog for every admitted leading name character',async()=>{
  for(const name of ['plan','1plan','_plan','.plan','-plan']) {
    const context={connection:{drafts:{key:()=>['endpoint','local','main','session']}},notify:assert.fail};
    const pane=new(createPaneClass(context))('main');let reads=0;
    Object.assign(pane,{generation:'one',renderComposer(){},async drainReferenceReleases(){},async action(action){assert.equal(action,'commands');reads++;}});
    pane.editors.set(JSON.stringify(context.connection.drafts.key()),{dirty:true,record:{header:'header'},text:`/${name} input`});
    await pane.bindDraft({session:'session',header:'header'});
    assert.equal(reads,1,name);
  }
});
test('typing a command fetches one catalog while the request is pending',async()=>{
  const context={connection:{},notify:assert.fail},pane=new(createPaneClass(context))('main');
  const request=Promise.withResolvers();let reads=0;
  const editor={text:'',textBytes:0,edit(text){this.text=text;}};
  Object.assign(pane,{generation:'one',editor,staged:{data:{}},action(){reads++;return request.promise;}});
  pane.edit('hello');assert.equal(reads,0);
  pane.edit('/plan');pane.edit('/plan on');assert.equal(reads,1);
  request.resolve();await request.promise;
});
test('a replaced connection can fetch its catalog without an old read clearing the new owner',async()=>{
  const context={connection:{},notify:assert.fail},pane=new(createPaneClass(context))('main');
  const requests=[];
  Object.assign(pane,{generation:'one',staged:{data:{}},action(){const request=Promise.withResolvers();requests.push(request);return request.promise;}});
  pane.prefetchCommands('/plan');context.connection={};pane.prefetchCommands('/plan');
  assert.equal(requests.length,2);requests[0].resolve();await new Promise(resolve=>setImmediate(resolve));
  pane.prefetchCommands('/plan on');assert.equal(requests.length,2);
  requests[1].resolve();await requests[1].promise;
});
test('argument claims retain the tail, require a leading trigger and remain visibly reversible',()=>{
  assert.equal(commandSelection('  /pl review this',2,5,plan,true).text,'/plan review this');
  assert.equal(commandSelection('existing message',0,0,plan).text,'/plan existing message');
  assert.equal(commandSelection('existing message',8,8,plan).text,'/plan existing message');
  assert.equal(commandSelection('existing message',0,8,plan).text,'/plan existing message');
  assert.equal(commandSelection('/plan keep arguments',8,8,plan).text,'/plan keep arguments');
  assert.throws(()=>commandSelection('existing /pl message',9,12,plan,true),/start of the message/);
  assert.deepEqual(claimedCommand('/plan existing message',[plan]),{name:'plan',hint:plan.composer_input.hint,end:6});
  assert.equal(claimedCommand('ordinary prose',[plan]),undefined);
  assert.deepEqual(claimedCommand('/export out.md',[]),{name:'export',hint:'[PATH] [-f markdown|md|json] [-i includes]',end:8});
  const none={name:'help',composer_input:{kind:'none'}};
  assert.deepEqual(commandSelection('selected prose',0,14,none),{kind:'execute',consume:undefined});
  assert.deepEqual(commandSelection('prefix /help tail',7,12,none,true),{kind:'execute',consume:{start:7,end:12}});
  assert.throws(()=>commandSelection('',0,0,{name:'internal',composer_input:null}),/dedicated controls/);
});

test('menu selection consumes its captured revision so another explicit selection can succeed',async()=>{
  const pane=new(createPaneClass({}))('main');
  const editor={revision:0,text:'original'};
  Object.assign(pane,{generation:'one',editor,input:{value:editor.text,selectionStart:0,selectionEnd:0,focus(){},setSelectionRange(){}},
    edit(text){editor.text=text;editor.revision++;},closeCompletions(){}});
  pane.captureCommandSelection();await pane.selectCommand(plan);
  pane.input.value=editor.text;
  await pane.selectCommand(plan);
  assert.equal(editor.text,'/plan original');
  pane.captureCommandSelection();editor.revision++;
  await assert.rejects(pane.selectCommand(plan),/input changed/);
  await pane.selectCommand(plan);
  assert.equal(editor.text,'/plan original');
});

test('the actual command-menu handler preserves prose and refuses stale or IME selections',async()=>{
  const Pane=createPaneClass({});
  const pane=new Pane('main');
  const editor={revision:0,text:'existing message'};
  Object.assign(pane,{generation:'one',editor,input:{value:editor.text,selectionStart:8,selectionEnd:16,focus(){},setSelectionRange(){}},
    edit(text){editor.text=text;editor.revision++;},closeCompletions(){}});
  pane.captureCommandSelection();await pane.selectCommand(plan);
  assert.equal(editor.text,'/plan existing message');
  pane.captureCommandSelection();editor.revision++;
  await assert.rejects(pane.selectCommand(plan),/input changed/);
  pane.captureCommandSelection();pane.composing=true;
  await assert.rejects(pane.selectCommand(plan),/input changed/);
});

test('completion dispatch retains the picked revision even when its action runs later',async()=>{
  const pane=new (createPaneClass({}))('main');let action;
  const token={text:'/pl suffix',cursor:3,start:0,end:3};
  const editor={revision:1,text:token.text};
  Object.assign(pane,{generation:'one',editor,completionState:{...token,generation:'one'},completionToken:()=>token,
    closeCompletions(){},input:{value:token.text},edit:assert.fail});
  pickCompletion(pane,{perform(run){action=run;}},{...plan,group:'command'});
  editor.revision++;
  await assert.rejects(action(),/input changed/);
});

test('confirmed token consumption retires command selections captured before its text update',async()=>{
  const store=await DraftStore.open('a'.repeat(32),{kind:'local'});
  const editor=new DraftEditor(store,await store.ensure('main','session','b'.repeat(64)));
  editor.edit('/help retained text');await editor.flush();
  const pane=new(createPaneClass({}))('main');
  Object.assign(pane,{generation:'one',editor,input:{value:editor.text,selectionStart:0,selectionEnd:5}});
  pane.captureCommandSelection();
  await editor.update(record=>store.startDispatch(record,{kind:'command',id:'help-request',opaque:'request',text_bytes:0,images:0,captures:[]},true,{start:0,end:5}));
  await editor.settle(editor.record,{status:'complete',receipt:'done'});
  assert.equal(editor.text,' retained text');pane.input.value=editor.text;
  await assert.rejects(pane.selectCommand(plan),/input changed/);
  assert.equal((await store.get('main','session')).text,' retained text');
});
