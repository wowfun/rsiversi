// A complete frame owns its bytes; fragments never copy the accumulated prefix.
export class NullFrameReader {
  constructor(maximum) { this.buffer=Buffer.allocUnsafe(maximum); this.used=0; }
  push(chunk,receive) {
    let at=0;
    while(at<chunk.length) {
      const end=chunk.indexOf(0,at),stop=end<0?chunk.length:end,length=stop-at;
      if(this.used+length>this.buffer.length)throw new Error('CDP frame exceeds bound');
      chunk.copy(this.buffer,this.used,at,stop);this.used+=length;
      if(end<0)return;
      const frame=Buffer.from(this.buffer.subarray(0,this.used));this.used=0;
      receive(frame);at=end+1;
    }
  }
}

