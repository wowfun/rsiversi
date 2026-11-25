import test from 'node:test';
import assert from 'node:assert/strict';
import {createQueueEditor} from '../src/runtime/queue-editor.js';
import {createPaneClass} from '../src/runtime/session-controller.js';

test('queued replacement preserves opaque blocks and freezes the admitted edit',async()=>{
  const image={type:'image',media:{digest:'a'.repeat(64),width:120}},reference={type:'reference',reference:{preview:'Frozen',snapshot:{capture_id:'capture'}}};
  const file={type:'file',file:{id:'f'.repeat(64),name:'queue-notes.txt',bytes:1536}};
  const content=[{type:'text',text:'Original'},image,file,reference];
  let received,release,closed=0;
  const editor=createQueueEditor({content,editable:true,changed(){},closed(){closed++;},submit:async value=>{received=value;await new Promise(resolve=>release=resolve);}});
  editor.edit(editor.blocks[0].key,'Edited');
  const saving=editor.save();
  editor.remove(editor.blocks[1].key);editor.edit(editor.blocks[0].key,'Too late');editor.addText();
  await editor.save();
  assert.deepEqual(received,[{type:'text',text:'Edited'},image,file,reference]);
  assert.equal(editor.blocks.length,4);assert.equal(content[0].text,'Original');
  release();await saving;assert.equal(closed,1);
  editor.close();assert.equal(closed,1);
});

test('a refused queue edit retains the editable contents and its error; closing fences late completion',async()=>{
  let reject,changes=0;
  const editor=createQueueEditor({content:[{type:'text',text:'Retain me'}],editable:true,changed(){changes++;},closed(){},submit:()=>new Promise((_,fail)=>reject=fail)});
  const work=editor.save();reject(new Error('not admitted'));await assert.rejects(work,/not admitted/);
  assert.equal(editor.status,'not admitted');assert.equal(editor.saving,false);assert.equal(editor.blocks[0].value.text,'Retain me');
  const retry=editor.save();editor.close();const before=changes;reject(new Error('owner retired'));await assert.rejects(retry,/owner retired/);assert.equal(changes,before);
});

test('read-only pending input cannot mutate or submit retained content',async()=>{
  const editor=createQueueEditor({content:[{type:'text',text:'Read only'}],editable:false,changed:assert.fail,closed(){},submit:assert.fail});
  editor.edit(editor.blocks[0].key,'Changed');editor.remove(editor.blocks[0].key);editor.addText();await editor.save();
  assert.deepEqual(editor.blocks.map(block=>block.value),[{type:'text',text:'Read only'}]);
});

test('queue reads follow the last selection through a frame update and retire replaced messages',async t=>{
  const previous=globalThis.document;
  globalThis.document={activeElement:null};
  t.after(()=>{if(previous===undefined)delete globalThis.document;else globalThis.document=previous;});
  const reads=[];
  const pane=new (createPaneClass({connection:{},call(_,input){
    const reply=Promise.withResolvers();
    const {queue}=JSON.parse(input);
    reads.push(text=>reply.resolve(JSON.stringify(queue.items.map(()=>[{type:'text',text}]))));
    return reply.promise;
  }}))('main');
  const items=['first','next'].map(id=>({queue_slot:{id},message_id:id,editable:true}));
  pane.generation='1';
  const data={generation:'1',queue:items};
  pane.staged={data};
  const bind=()=>pane.bindActions(undefined,{generation:'1',items:data.queue});
  pane.bindActions(undefined,{generation:'1',items:[items[0]]});
  const preview=pane.readQueueContent('first','first');
  const first=pane.queueAction('first','read');
  bind();
  const next=pane.queueAction('next','read');
  assert.equal(reads.length,2,'preview and editor share one acceptance read');
  pane.bindActions(undefined,undefined);
  reads[1]('Selected second');
  await next;
  assert.equal(pane.queueDialog?.inlineSlot,'next');
  reads[0]('Selected first');
  await first;
  assert.equal((await preview)[0].text,'Selected first');
  assert.equal(pane.queueDialog.inlineSlot,'next');
  bind();
  assert.equal((await pane.readQueueContent('first','first'))[0].text,'Selected first');
  data.queue=[{...items[0],message_id:'replacement'}];
  pane.renderQueue(data);
  bind();
  await assert.rejects(pane.readQueueContent('first','first'),/current queue view/);
  const retired=pane.queueAction('first','read');
  data.queue=[];
  pane.renderQueue(data);
  reads[2]('Removed from queue');
  await retired;
  assert.equal(pane.queueDialog,undefined);
});

test('one expanded queue reads all missing visible slots together and reuses their content',async()=>{
  const requests=[];
  const pane=new (createPaneClass({connection:{},async call(_,input){
    const {items}=JSON.parse(input).queue;
    requests.push(items);
    return JSON.stringify(items.map(({expected_message_id:text})=>[{type:'text',text}]));
  }}))('main');
  pane.generation='1';
  const items=Array.from({length:64},(_,i)=>({queue_slot:{id:'slot-'+i},message_id:'v-'+i}));
  pane.bindActions(undefined,{generation:'1',items});
  const read=()=>Promise.all(items.map(item=>pane.readQueueContent(item.queue_slot.id,item.message_id)));
  assert.deepEqual((await read()).map(row=>row[0].text),items.map(row=>row.message_id));
  await read();
  assert.equal(requests.length,1);
  assert.equal(requests[0].length,64);
  items[17]={...items[17],message_id:'replaced'};
  pane.renderQueue({queue:items});
  assert.equal((await read())[17][0].text,'replaced');
  assert.deepEqual(requests[1],[{slot_id:'slot-17',expected_message_id:'replaced'}]);
  pane.renderQueue({protected:true});
  assert.equal(pane.queueContents.size,0);
});
