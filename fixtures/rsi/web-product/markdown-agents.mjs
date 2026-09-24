import assert from 'node:assert/strict';
import {mkdir,writeFile,rm} from 'node:fs/promises';
import {join} from 'node:path';
import {waitUntil} from './service.mjs';
export async function verifyMarkdownAgents(page,service,report,browser,bodies) {
 const directory=join(service.workspace,'.agents','agents');await mkdir(directory,{recursive:true});
 const path=join(directory,'reviewer.md');
 const definition=version=>`---\ndescription: Review isolated source ${version}\nallow: []\n---\nMARKDOWN-REVIEWER-V${version}. Read the supplied task and return a concise review.\n`;
 await writeFile(path,definition(1));
 const overflow=Array.from({length:32},(_,index)=>join(directory,`zz-overflow-${String(index).padStart(2,'0')}.md`));
 for(const file of overflow)await writeFile(file,'---\ndescription: bounded catalog entry\n---\nDo the requested work.');
 const pane=page.locator('[aria-label="Main conversation"]'),editor=pane.getByRole('textbox',{name:'Main message'});
 await editor.fill('@rev');
 await pane.getByRole('option').filter({hasText:'reviewer'}).waitFor();
 await page.screenshot({path:join(report,`${browser}-agent-completion.png`)});
 await editor.press('Tab');assert.match(await editor.inputValue(),/^@reviewer/);
 for(const text of ['\\@reviewer','`@reviewer','```\n@reviewer','@.foo','@path_hex:abcd','@a/b']) {
   await editor.fill(text);assert.equal(await editor.getAttribute('aria-expanded'),'false',text);
 }
 await editor.fill('@reviewer: please check');
 await editor.evaluate(element=>{element.setSelectionRange(9,9);element.dispatchEvent(new KeyboardEvent('keyup',{key:'ArrowLeft',bubbles:true}));});
 await pane.getByRole('option').filter({hasText:'reviewer'}).waitFor();await editor.press('Escape');
 const send=async text=>{const before=bodies.length;await editor.fill(text);await pane.getByRole('button',{name:'Send ↗',exact:true}).click();await waitUntil(()=>bodies.length>before,'provider entered');await pane.locator('.pane-status').filter({hasText:'Completed'}).waitFor();};
 await send('@reviewer: delegate to Markdown reviewer, first task');
 const child=version=>bodies.find(body=>body.messages.some(message=>message.role==='system'&&JSON.stringify(message.content).includes(`MARKDOWN-REVIEWER-V${version}`)));
 await waitUntil(()=>!!child(1),'first custom reviewer');assert.equal(child(1).tools?.length??0,0,'empty allow must expose no ordinary tools');
 await writeFile(path,definition(2));
 await send('@reviewer delegate to Markdown reviewer, second task');
 await waitUntil(()=>!!child(2),'next spawn reads edited definition');
 assert(bodies.some(body=>JSON.stringify(body.messages).includes('Review isolated source 2')),'existing parent did not receive current catalog');
 await writeFile(path,'---\ndescription: broken\nunknown_field: true\n---\nInvalid\n');
 await editor.fill('@rev');await pane.getByRole('option').filter({hasText:'reviewer'}).waitFor();
 assert.match(await pane.getByRole('option').filter({hasText:'reviewer'}).innerText(),/Unavailable/);
 await editor.press('Escape');await editor.fill('');await rm(path);for(const file of overflow)await rm(file);
 return {catalog_overflow_keeps_available_roles:true,next_spawn_refresh:true,empty_allow:true,malformed_definition_diagnostic:true,provider_calls:bodies.length};
}
