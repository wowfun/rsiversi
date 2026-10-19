import {spawn} from 'node:child_process';
import {access} from 'node:fs/promises';
import {dirname,resolve} from 'node:path';
import {fileURLToPath} from 'node:url';

const root=resolve(dirname(fileURLToPath(import.meta.url)),'../../..');
async function waitClosed(closed) {
  let timer;
  try {await Promise.race([closed,new Promise((_,reject)=>{timer=setTimeout(()=>reject(Error('Fixture lease cleanup timed out')),5000);})]);}
  finally {clearTimeout(timer);}
}
export async function holdGeneration(bundle, environment=process.env, launch=spawn) {
  const binary=environment.RSI_APP_TOOLS_BINARY ?? resolve(root,`target/debug/rsi-app-tools${process.platform==='win32'?'.exe':''}`);
  await access(binary).catch(()=>{throw Error('Build the fixture lease helper: cargo build --locked -p rsi-app-tools, or set RSI_APP_TOOLS_BINARY');});
  const helper=launch(binary,['fixture-lease',bundle],{stdio:['pipe','pipe','pipe'],windowsHide:true});
  let closing=false,diagnostic='',ready=false;
  helper.stderr.on('data',bytes=>{diagnostic=(diagnostic+bytes.toString()).slice(-4096);});
  const closed=new Promise(resolve=>helper.once('close',resolve));
  const fail=error=>{if(!closing) {closing=true;helper.stdin.destroy();helper.kill();} return error;};
  const reap=async()=>{
    try {await waitClosed(closed);}
    catch(error) {
      helper.kill('SIGKILL');
      try {await waitClosed(closed);} catch(cleanup) {error.cause=cleanup;}
      throw error;
    }
  };
  try {
    await new Promise((resolve,reject)=>{
      let output='';
      const timer=setTimeout(()=>reject(fail(Error('Fixture lease readiness timed out'))),30000);
      const done=error=>{clearTimeout(timer);if(error)reject(fail(error));else resolve();};
      helper.once('error',error=>done(error));
      helper.once('close',(code,signal)=>{
        const error=Error(`Fixture generation lease exited early (${code??signal}): ${diagnostic}`);
        if(!ready)done(error);
        else if(!closing){process.exitCode=1;console.error(error.message);}
      });
      helper.stdout.on('data',bytes=>{
        if(ready){process.exitCode=1;console.error('Unexpected fixture lease output');fail(Error('Unexpected fixture lease output'));return;}
        if(output.length+bytes.length>32){done(Error('Invalid fixture lease readiness'));return;}
        output+=bytes.toString('utf8');
        if(output.includes('\n')) {
          if(output!=='ready\n'){done(Error('Invalid fixture lease readiness'));return;}
          ready=true;done();
        }
      });
    });
  } catch(error) {
    fail(error);
    try {await reap();} catch(cleanup) {error.cause=cleanup;error.message+=`; cleanup: ${cleanup.message}`;}
    throw error;
  }
  // EOF at normal Node exit releases the helper; pipes must not keep Node alive.
  helper.unref();helper.stdin.unref();helper.stdout.unref();helper.stderr.unref();
  return {async close(){
    closing=true;
    // Explicit cleanup must keep Node alive until EOF has actually reaped the helper.
    helper.ref();helper.stdout.ref();helper.stderr.ref();
    helper.stdin.end();
    await reap();
  }};
}
