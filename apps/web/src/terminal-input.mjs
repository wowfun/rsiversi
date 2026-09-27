import {backpressure} from "./terminal-read.mjs";
// A bounded document queue forwards each byte batch once. Rust owns all receipts.
export class TerminalInput {
  constructor(send,failure,wait=ms=>new Promise(resolve=>setTimeout(resolve,ms))){this.wait=wait;this.send=send;this.failure=failure;this.buffer=undefined;this.head=0;this.length=0;this.working=false;this.stopped=false;}
  get queuedBytes(){return this.length;}
  push(bytes){
    if(this.stopped)return;
    if(this.length+bytes.length>65536){this.stop('Input queue is full. Check the terminal before typing again.');return;}
    if(!bytes.length)return;
    this.buffer??=new Uint8Array(65536);
    const tail=(this.head+this.length)%65536,first=Math.min(bytes.length,65536-tail);
    this.buffer.set(bytes.subarray(0,first),tail);this.buffer.set(bytes.subarray(first),0);
    this.length+=bytes.length;void this.drain();
  }
  stop(message){if(this.stopped)return;this.stopped=true;this.buffer=undefined;this.length=0;if(message)this.failure(message);}
  async drain(){
    if(this.working||this.stopped)return;this.working=true;
    try{while(this.length&&!this.stopped){
      const bytes=new Uint8Array(this.length),first=Math.min(this.length,65536-this.head);
      bytes.set(this.buffer.subarray(this.head,this.head+first));bytes.set(this.buffer.subarray(0,this.length-first),first);
      for(;;){try{await this.send(bytes);break;}catch(error){if(!backpressure(error))throw error;await this.wait(250);if(this.stopped)return;}}
      if(!this.stopped){this.head=(this.head+bytes.length)%65536;this.length-=bytes.length;}
    }}
    catch(error){this.stop(String(error));}finally{this.working=false;}
  }
}
