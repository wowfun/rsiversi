import {backpressure} from "./terminal-read.mjs";
// A bounded document queue forwards each byte batch once. Rust owns all receipts.
export class TerminalInput {
  constructor(send,failure,wait=ms=>new Promise(resolve=>setTimeout(resolve,ms))){this.wait=wait;this.send=send;this.failure=failure;this.queue=new Uint8Array();this.working=false;this.stopped=false;}
  push(bytes){
    if(this.stopped)return;
    if(this.queue.length+bytes.length>65536){this.stop('Input queue is full. Check the terminal before typing again.');return;}
    const next=new Uint8Array(this.queue.length+bytes.length);next.set(this.queue);next.set(bytes,this.queue.length);this.queue=next;void this.drain();
  }
  stop(message){if(this.stopped)return;this.stopped=true;this.queue=new Uint8Array();if(message)this.failure(message);}
  async drain(){
    if(this.working||this.stopped)return;this.working=true;
    try{while(this.queue.length&&!this.stopped){const bytes=this.queue.slice();for(;;){try{await this.send(bytes);break;}catch(error){if(!backpressure(error))throw error;await this.wait(250);if(this.stopped)return;}}if(!this.stopped)this.queue=this.queue.slice(bytes.length);}}
    catch(error){this.stop(String(error));}finally{this.working=false;}
  }
}
