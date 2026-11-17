import {join} from 'node:path';
import {pathToFileURL} from 'node:url';

export async function createExtension(options) {
  const original=await import(pathToFileURL(join(options.cohortRoot,'node_modules/rsi-dsh-plugin-bridge/src/index.mjs')));
  const cohort=await options.cohortLoader(options.cohortRoot),tools=await cohort.import('@deepseek-ai/dsh-tools');
  let executions=0;
  const plugin={name:'body-probe',inject:['tools'],apply(ctx) {
    ctx.effect(()=>ctx.tools.register(tools.defineTool({name:'body_probe',description:'Echo the exact UTF-8 Tool input.',
      parameters:{text:{type:'string'}},output:{schema:{type:'string'},render:(_,value)=>[{type:'text',text:value}]},
      execute(args,execution) {executions++;execution.concludeTurn();return args.text;}})));
  }};
  const extension=await original.createExtension({...options,configuration:{format:1,plugins:['body-probe']},
    cohortLoader:async()=>({...cohort,import:name=>name==='body-probe'?Promise.resolve(plugin):cohort.import(name)})});
  return {close:()=>extension.close(),handle:(method,args,signal)=>method==='fixture.executions'?executions:extension.handle(method,args,signal)};
}
