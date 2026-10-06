import test from 'node:test';
import assert from 'node:assert/strict';
import {readFile} from 'node:fs/promises';
import vm from 'node:vm';
const source=await readFile(new URL('../app.js',import.meta.url),'utf8');
const context={button:(label,run)=>({label,run})};vm.runInNewContext(source.slice(source.indexOf('class Pane {'),source.indexOf('function basename('))+';this.Pane=Pane;',context);
test('protected projection clears cached queue controls and closes its active editor',()=>{
 const items=[{message_id:'queued'}];let children=1,closed=0;
 const pane=Object.assign(Object.create(context.Pane.prototype),{queueItems:items,queueGeneration:'1',queueBinding:{generation:'1'},queueList:{hidden:false,replaceChildren(){children=0;}},queueDialog:{close(){closed++;}}});
 pane.renderQueue({queue:items,generation:'1',protected:true});
 assert.equal(pane.queueList.hidden,true);assert.equal(children,0);assert.equal(closed,1);
 assert.equal(pane.queueBinding,undefined);assert.equal(pane.queueItems,undefined);
});
test('protection replaces cached pending interaction actions and ordinary panes can recover them',()=>{
 let rendered=[],actions=0;
 const pane=Object.assign(Object.create(context.Pane.prototype),{waiting:{replaceChildren(...items){rendered=items;}},action(){actions++;}});
 const pending=[{kind:'question',title:'Choose',owner:'session',id:'question'}];
 pane.renderPending({pending,protected:false});assert.equal(rendered.length,1);rendered[0].run();assert.equal(actions,1);
 pane.renderPending({pending,protected:true});assert.equal(rendered.length,0);
 pane.renderPending({pending,protected:false});assert.equal(rendered.length,1);
});
test('acknowledgement leaves protected stop and queue bindings absent after deferred rendering',async()=>{
 let release,entered;
 const rendering=new Promise(resolve=>{release=resolve;});
 const ready=new Promise(resolve=>{entered=resolve;});
 const control={disabled:false};
 const pane={kind:'native',cancel:{disabled:false,hidden:false},queueList:{querySelectorAll(){return [control];}}};
 const current={closing:false,async render(){entered();await rendering;return {};}};
 const scope={connection:current,panes:new Map([['left',pane]]),render(){},rendererSlots:{},notify(){},view:null};
 vm.runInNewContext(source.slice(source.indexOf('let frameId;'),source.indexOf('function createDocumentConnection('))+';this.presentFrame=presentFrame;',scope);
 const frame={kind:'snapshot',frame_id:'1',view:{surfaces:{left:{generation:'1',active:'turn',queue:[{message_id:'queued'}],protected:true}}}};
 const work=scope.presentFrame(frame,{},current);await ready;
 assert.equal(pane.stopBinding,undefined);assert.equal(pane.queueBinding,undefined);
 release();assert.equal((await work).accepted,true);
 assert.equal(pane.stopBinding,undefined);assert.equal(pane.queueBinding,undefined);assert.equal(pane.cancel.hidden,true);assert.equal(control.disabled,true);
 frame.frame_id='2';frame.view.surfaces.left.protected=false;
 await scope.presentFrame(frame,{},current);
 assert.equal(pane.stopBinding.turn_id,'turn');assert.equal(pane.queueBinding.generation,'1');assert.equal(control.disabled,false);
});
