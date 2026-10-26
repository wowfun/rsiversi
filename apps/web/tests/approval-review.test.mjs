import test from 'node:test';
import assert from 'node:assert/strict';
import {approvalFields,approvalArguments,approvalReason,approvalJson,decisionAction} from '../approval-review.js';
test('approval describes supplied execution evidence without claiming enforcement',()=>{
  const review={cwd:'/project',sandbox:'workspace-write',arguments:{}};
  assert.deepEqual(approvalFields(null),[]);
  assert.deepEqual(approvalFields(review),[['Execution location','Not supplied'],['Working directory','/project'],['Requested sandbox policy','workspace-write']]);
  for(const [location,label] of [[{kind:'local'},'Service machine'],[{kind:'ssh',target:'server'},'SSH · server'],[{kind:'future'},'Unrecognized execution location']]) {
    assert.deepEqual(approvalFields({...review,execution:{binding:{location},workspace:'/workspace'}}),[['Execution location',label],['Working directory','/project'],['Workspace','/workspace'],['Requested sandbox policy','workspace-write']]);
  }
});
test('approval retains all ordered arguments including long, nested and non-object values',()=>{
  const value={command:'<script>'+'x'.repeat(65536),nested:{array:[true,null,'你好']}};
  const rows=approvalArguments(value);
  assert.deepEqual(rows.map(([key])=>key),['command','nested']);
  assert.deepEqual(Object.fromEntries(rows.map(([key,text])=>[key,JSON.parse(text)])),value);
  for(const value of [null,[],42,'text']) assert.deepEqual(approvalArguments(value),[['Value',JSON.stringify(value,null,2)]]);
});

test('machine reasons have plain presentation while custom reasons remain exact',()=>{
  assert.equal(approvalReason('Agent turn message-abc123 requested this Tool effect'),'The assistant requested this tool action.');
  assert.equal(approvalReason('Read the selected workspace file.'),'Read the selected workspace file.');
  assert.equal(approvalReason('Agent turn something else'),'Agent turn something else');
});

test('large approval arguments and raw JSON have independent bounded UTF-8 windows',()=>{
  const value={large:'界'.repeat(1024*1024),tail:'must be visibly omitted'};
  const fields=approvalArguments(value);
  const text=fields.flat().join('');
  assert.ok(new TextEncoder().encode(text).length<=256*1024);
  assert.match(text,/truncated/);
  assert.ok(!text.includes('\uFFFD'));
  const raw=approvalJson({review:{arguments:value}});
  assert.ok(new TextEncoder().encode(raw).length<=256*1024);
  assert.match(raw,/truncated/);
});

test('an exactly fitting final argument is not truncated to reserve a later-field marker',()=>{
  const value={last:'x'.repeat(256*1024-6)};
  assert.deepEqual(approvalArguments(value),[['last',JSON.stringify(value.last)]]);
});


test('human decisions refuse duplicate clicks and recover only known admission refusals',async()=>{
  let calls=0, reject, disabled; const pending=[],reports=[];
  const decide=decisionAction(()=>{calls++;return new Promise((_,fail)=>{reject=fail})},value=>{disabled=value},text=>reports.push(text),value=>pending.push(value));
  const first=decide(true); await decide(false); assert.equal(calls,1); assert.equal(disabled,true);
  reject(Object.assign(new Error('busy'),{notAdmitted:true})); await first;
  assert.equal(disabled,false);
  const retry=decide(true); assert.equal(calls,2);
  reject(new Error('outcome unknown')); await retry;
  await decide(false); assert.equal(calls,2); assert.equal(disabled,true);
  assert.deepEqual(pending,[true,false,true,false]);
  assert.deepEqual(reports.slice(0,3),['','busy','']);
  assert.match(reports[3],/outcome unknown.*Refresh request/);
});

test('request refresh coalesces clicks and keeps decisions disabled until fresh presentation', async () => {
  const {readFile} = await import('node:fs/promises'); const {default:vm} = await import('node:vm');
  const source = await readFile(new URL('../app.js', import.meta.url), 'utf8');
  const start = source.indexOf('function interactionRefresh(');
  const end = source.indexOf('const settingsDialog =', start);
  let resolve, reject, calls=0;
  const controls=[{disabled:false},{disabled:true},{disabled:false}];
  const scope = {button:(_label,run)=>({run}), form:{setAttribute(){},querySelectorAll:()=>controls},
    failure:{}, base:{}, command:()=>{calls++;return new Promise((done,fail)=>{resolve=done;reject=fail;});}};
  vm.runInNewContext(source.slice(start,end)+';this.refresh=interactionRefresh(form,base,"owner",failure,command);',scope);
  const first=scope.refresh.run(); await scope.refresh.run();
  assert.equal(calls,1); assert.equal(scope.refresh.disabled,true);
  assert.ok(controls.every(choice=>choice.disabled));
  resolve(); await first; assert.equal(scope.refresh.disabled,false);
  for (const disabled of [false,true]) {
    controls.forEach(choice=>{choice.disabled=disabled;});
    const refreshing=scope.refresh.run(); reject(new Error('offline')); await refreshing;
    assert.ok(controls.every(choice=>choice.disabled===disabled));
    assert.match(scope.failure.textContent,/Pending-state read failed/);
  }
});
