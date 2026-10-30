import test from 'node:test';
import assert from 'node:assert/strict';
import {createPaneClass} from '../src/runtime/session-controller.js';
import {createFramePresenter} from '../src/runtime/frame-presenter.js';
import {FrameTicket} from '../src/runtime/frame-ticket.js';
const context={button:(label,run)=>({label,run,dataset:{}})};
context.Pane=createPaneClass(context);

test('protected projection clears queue authority and closes its active editor',()=>{
 const pane=new context.Pane('main');let closed=0;
 pane.queueBinding={generation:'1'};pane.queueDialog={close(){closed++;}};
 pane.renderQueue({queue:[{message_id:'queued'}],generation:'1',protected:true});
 assert.equal(closed,1);assert.equal(pane.queueBinding,undefined);
});

test('acknowledgement leaves protected stop and queue bindings absent after deferred rendering',async()=>{
 let release,entered;
 const rendering=new Promise(resolve=>{release=resolve;});
 const ready=new Promise(resolve=>{entered=resolve;});
 const pane=new context.Pane('left');pane.node={isConnected:true};
 const current={closing:false,async render(){entered();await rendering;return {};}};
 const scope={connection:current,panes:new Map([['left',pane]]),render(){},rendererSlots:[],notify(){},view:null,$:()=>({isConnected:true}),presentation:{begin:(owner,id)=>new FrameTicket(owner,id,assert.fail),commit(ticket,render,hosts){render();ticket.requireHosts(hosts());ticket.commit(ticket.epoch,ticket.frameId);return ticket.committed}}};
 scope.presentFrame=createFramePresenter(scope).presentFrame;
 const frame={kind:'snapshot',frame_id:'1',view:{surfaces:{left:{generation:'1',active:'turn',queue:[{message_id:'queued'}],protected:true}}}};
 const work=scope.presentFrame(frame,{},current);await ready;
 assert.equal(pane.stopBinding,undefined);assert.equal(pane.queueBinding,undefined);
 release();assert.equal((await work).accepted,true);
 assert.equal(pane.stopBinding,undefined);assert.equal(pane.queueBinding,undefined);
 frame.frame_id='2';frame.view.surfaces.left.protected=false;
 await scope.presentFrame(frame,{},current);
 assert.equal(pane.stopBinding.turn_id,'turn');assert.equal(pane.queueBinding.generation,'1');
});
