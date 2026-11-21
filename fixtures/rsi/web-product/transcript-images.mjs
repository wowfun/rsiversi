// A paired browser reads local Markdown images from the conversation's execution host.
import assert from 'node:assert/strict';
import {mkdir,open,symlink,writeFile} from 'node:fs/promises';
import {dirname,join,resolve} from 'node:path';
import {chromium,firefox} from 'playwright';
import {connectWorkbench,readBrowserCaller,registerFixtureWorkspace} from './browser-fixture.mjs';
import {startService} from './service.mjs';
import {readDeepseekKey,deepseekProfile} from './live-limits.mjs';
import {png,unreadablePng} from './png.mjs';
import {verifyAttachmentRail} from './images.mjs';

const report=resolve(process.env.RSI_REPORT_DIR);
const live=process.env.RSI_LIVE_DEEPSEEK==='1';
const binary=resolve(process.env.RSI_WEB_BINARY),assets=resolve(process.env.RSI_WEB_ASSETS);
for(const [name,engine] of Object.entries(live?{chromium}:{chromium,firefox})){
  const dir=join(report,name);await mkdir(dir,{recursive:true});
  const service=await startService({binary,assets,report:dir,developmentHttp:true,foregroundHost:process.platform==='win32',
    registrationLabel:'Transcript image author',deepseekKey:live?await readDeepseekKey('.local/dev/.env'):undefined,
    async configure({config,workspace}){
      await writeFile(join(workspace,'图 #1%.png'),png(84,52,[64,128,72,255]));
      await writeFile(join(dirname(workspace),'.outside.png'),png(210,90,[175,88,46,255]));
      await writeFile(join(dirname(workspace),'外部 草图.png'),png(210,90,[85,102,160,255]));
      await writeFile(join(dirname(workspace),'linked.png'),png(210,90,[52,132,165,255]));
      await writeFile(join(workspace,'drawing.svg'),'<svg xmlns="http://www.w3.org/2000/svg" width="96" height="48"><circle cx="40" cy="24" r="21" fill="teal"/></svg>');
      await writeFile(join(workspace,'ordinary.png'),'An ordinary text document');
      await writeFile(join(workspace,'notes.md'),'First line\nSelected source line\nLast line');
      await writeFile(join(dirname(workspace),'outside.txt'),'Fixture outside document');
      const sibling=join(dirname(workspace),'sibling-documents');
      await mkdir(sibling);
      await writeFile(join(sibling,'linked.txt'),'Fixture linked document');
      await symlink(sibling,join(workspace,'linked-directory'),process.platform==='win32'?'junction':'dir');
      await writeFile(join(dirname(workspace),'unreadable.png'),unreadablePng());
      const large=await open(join(workspace,'large.png'),'w');
      try{await large.truncate(50*1024*1024+1);}finally{await large.close();}
      if(live){
        await writeFile(join(config,'settings.json'),JSON.stringify({'rsi.agent':{default_model:{deployment:'live',model:'deepseek-flash'},default_reasoning_effort:'off'}}));
        await writeFile(join(config,'host-profiles/fixture/host.profile.toml'),deepseekProfile('deepseek-flash',{context_window_tokens:1000000,default_output_reserve_tokens:4096,max_output_reserve_tokens:8192}));
      }
    }});
  let browser,page;
  const result={live,errors:[],observations:[],unreadableResponses:[]};
  try{
    browser=await engine.launch();
    async function connect(registration,width){
      const ctx=await browser.newContext({viewport:{width,height:960},locale:'en-US',colorScheme:'dark'});
      page=await ctx.newPage();page.setDefaultTimeout(30000);
      page.on('pageerror',e=>result.errors.push(e.message));
      page.on('response',response=>{
        if(new URL(response.url()).pathname.endsWith('/files/image/2/read') &&
          response.request().postDataJSON()?.input===Buffer.from('../unreadable.png').toString('hex'))
          result.unreadableResponses.push(response.status());
      });
      await connectWorkbench(page,service,registration);
      assert.deepEqual(await readBrowserCaller(page,registration.id),{status:200,device:registration.id});
      return ctx;
    }
    const first=service.registration;
    const setup=await browser.newContext();
    try{await registerFixtureWorkspace(setup.request,service);}finally{await setup.close();}
    const author=await connect(first,1440);
    const abs=join(dirname(service.workspace),'.outside.png');
    const markdown=[
      '**注意：**内容',
      '![Workspace diagram](./%E5%9B%BE%20%231%25.png)',
      '![Parent image](../.outside.png)',
      '![Spaced image](../外部 草图.png)',
      '![Absolute image][diagram]',
      '[diagram]: <'+abs+'>',
      '![Vector drawing](drawing.svg)',
      '![Missing](missing.png)',
      '![Wrong format](ordinary.png)',
      '![Too big](large.png)',
      '![](../unreadable.png)',
      '[Open drawing](../linked.png)',
      '[Source line](./notes.md#L2)',
      '[Absolute source](<'+join(service.workspace,'notes.md')+'#L3>)',
      '[Outside document](../outside.txt)',
      '[![Thumbnail](drawing.svg)](../linked.png)',
    ].join('\n\n');
    await page.getByRole('textbox',{name:'Main message'}).fill((live?'Reproduce the following Markdown verbatim, including the empty alt text and both final links. Output only the Markdown. No tools.':'Show these images and links.')+'\n\n'+markdown);
    await page.getByTestId('composer-send').click();
    const session=()=>page.locator('.pane.selected').getAttribute('data-session-id');
    async function inspect(stage,width){
      const article=page.locator('.pane.selected .message.assistant .markdown').last();
      const sizes={};
      for(const [label,w,h] of [['Workspace diagram',84,52],['Parent image',width,90],['Spaced image',width,90],['Absolute image',width,90],['Vector drawing',96,48]]){
        const trigger=article.getByRole('button',{name:'View full image: '+label,exact:true});
        // The placeholder can be below the fold before any image request starts.
        const placeholder=article.getByText(label,{exact:true});
        await trigger.or(placeholder).first().evaluate(el=>el.scrollIntoView({block:'nearest'}),undefined,{timeout:live?120000:30000});
        await trigger.waitFor({timeout:live?120000:30000});await trigger.scrollIntoViewIfNeeded();
        const handle=await trigger.getByRole('img').elementHandle();
        try {await page.waitForFunction((el)=>el.naturalWidth>0,handle);sizes[label]=await handle.evaluate(el=>[el.naturalWidth,el.naturalHeight]);}
        finally{await handle.dispose();}
        assert.deepEqual(sizes[label],[w,h]);
      }
      const emphasis=article.locator('strong').filter({hasText:'注意：'});
      assert.equal(await emphasis.count(),1);
      assert(await emphasis.evaluate(node=>node.textContent==='注意：' && node.nextSibling?.textContent?.startsWith('内容')));
      await emphasis.scrollIntoViewIfNeeded();
      await page.screenshot({path:join(dir,stage+'-emphasis.png')});
      for(const label of ['Missing','Wrong format','Too big']){
        const error=article.getByText('Image preview unavailable · '+label,{exact:true});
        if(!await error.count())await article.getByText(label,{exact:true}).scrollIntoViewIfNeeded().catch(()=>{});
        await error.waitFor();
      }
      const unreadable=article.getByText('Image preview unavailable · ../unreadable.png',{exact:true});
      if(!await unreadable.count())await article.getByText('../unreadable.png',{exact:true}).scrollIntoViewIfNeeded().catch(()=>{});
      await unreadable.waitFor();
      const trigger=article.getByRole('button',{name:'View full image: Absolute image'});
      await trigger.click();
      const dialog=page.getByRole('dialog',{name:'Image preview'});
      await dialog.getByRole('img').evaluate(el=>el.decode());
      await page.screenshot({path:join(dir,stage+'-preview.png')});
      await dialog.getByRole('button',{name:'Close image preview'}).press('Escape');
      assert(await trigger.evaluate(el=>el===document.activeElement));
      await page.screenshot({path:join(dir,stage+'.png')});
      const link=article.getByRole('button',{name:'Open drawing',exact:true});
      const wrapped=article.getByRole('button',{name:'Thumbnail',exact:true});
      assert.equal(await wrapped.getByRole('button').count(),0);
      await link.scrollIntoViewIfNeeded();
      await link.hover();
      const hovered=page.getByRole('img',{name:'../linked.png',exact:true});
      await hovered.waitFor();
      assert.deepEqual(await hovered.evaluate(async el=>{await el.decode();return [el.naturalWidth,el.naturalHeight];}),[width,90]);
      await page.screenshot({path:join(dir,stage+'-link-hover.png')});
      await page.keyboard.press('Escape');await hovered.waitFor({state:'hidden'});
      await link.focus();await link.press('Tab');await page.keyboard.press('Shift+Tab');await hovered.waitFor();
      await page.keyboard.press('Escape');await hovered.waitFor({state:'hidden'});
      await wrapped.click();
      const dock=page.getByRole('region',{name:'Resource workbench'}).filter({visible:true});
      const displayed=dock.locator('[data-image-preview] img');
      await displayed.waitFor();
      assert.deepEqual(await displayed.evaluate(async el=>{await el.decode();return [el.naturalWidth,el.naturalHeight];}),[width,90]);
      const imageBounds=await displayed.boundingBox();
      assert.deepEqual([imageBounds.width,imageBounds.height],[width,90]);
      await page.screenshot({path:join(dir,stage+'-link-open.png')});
      await writeFile(join(dirname(service.workspace),'linked.png'),png(width+8,90,[68,126,118,255]));
      await dock.getByRole('button',{name:'Reload file',exact:true}).click();
      await page.waitForFunction(w=>document.querySelector('[data-image-preview] img')?.naturalWidth===w,width+8);
      await dock.getByRole('button',{name:'Hide resources',exact:true}).click();
      await link.click();await displayed.waitFor();
      assert.equal(await dock.getByRole('tab').filter({hasText:'linked.png'}).count(),1);
      await dock.getByRole('button',{name:'Hide resources',exact:true}).click();
      const outside=article.getByText('Outside document',{exact:true});
      assert(await outside.isVisible());
      assert.equal(await outside.evaluate(el=>!!el.closest('button,a')),false);
      const response=page.waitForResponse(r=>new URL(r.url()).pathname.endsWith('/files/document/2/read'));
      await article.getByRole('button',{name:'Source line',exact:true}).click();
      const accepted=await response;
      assert.equal(accepted.status(),200);
      const file=dock.getByRole('region',{name:'Document file'});
      const line=stage==='author'?'Selected source line':'Changed since first read';
      await file.locator('mark').filter({hasText:line}).waitFor();
      assert.equal(await file.locator('pre').textContent(),'First line\n'+line+'\nLast line');
      await page.screenshot({path:join(dir,stage+'-document.png')});
      // The isolated device also exercises the Host boundary directly.
      const request=accepted.request();
      const headers=await request.allHeaders();
      delete headers['content-length'];
      const blocked=[];
      for(const path of ['../outside.txt',join(dirname(service.workspace),'outside.txt'),'linked-directory/linked.txt']){
        const denial=await page.context().request.post(request.url(),{
          headers,
          data:{...request.postDataJSON(),input:Buffer.from(path).toString('hex')},
        });
        blocked.push(denial.status());
        assert([400,422].includes(denial.status()));
        await denial.dispose();
      }
      await dock.getByRole('button',{name:'Hide resources',exact:true}).click();
      await article.getByRole('button',{name:'Absolute source',exact:true}).click();
      await file.locator('mark').filter({hasText:'Last line'}).waitFor();
      assert.equal(await dock.getByRole('tab').filter({hasText:'notes.md'}).count(),1);
      await dock.getByRole('button',{name:'Hide resources',exact:true}).click();
      (result.documents??=[]).push({stage,line,absoluteLine:3,blocked});
      result.observations.push({stage,sizes,unreadable:await unreadable.innerText(),imageLink:{hover:true,keyboard:true,dock:[imageBounds.width,imageBounds.height],reloadedWidth:width+8,tabs:1}});
    }
    await inspect('author',210);
    if(process.env.RSI_CHECK_IMAGE_DRAG==='1'){
      const reads=[],mediaReads=[];
      const watch=request=>{
        const path=new URL(request.url()).pathname;
        if(path.endsWith('/files/image/2/read'))reads.push(request.method());
        if(path.endsWith('/media/read/1/read'))mediaReads.push(request.method());
      };
      author.on('request',watch);
      try{
        const editor=page.getByRole('textbox',{name:'Main message'});
        const thumbnail=page.locator('.message.assistant').getByRole('button',{name:'View full image: Workspace diagram'}).getByRole('img');
        await thumbnail.dragTo(editor);
        const chip=page.locator('.pane.selected .draft-image');
        const preview=chip.getByRole('button',{name:'Preview image',exact:true});
        const image=preview.locator('img');
        await image.waitFor();assert.deepEqual(await image.evaluate(el=>[el.naturalWidth,el.naturalHeight]),[84,52]);
        const box=await image.boundingBox();
        assert(box && box.width>=60 && box.width<=64 && box.width===box.height);
        const editorBox=await editor.boundingBox();
        assert(editorBox && box.y+box.height<=editorBox.y);
        assert.equal(await editor.inputValue(),'');assert.equal(await page.getByRole('dialog',{name:'Image preview'}).count(),0);
        await page.screenshot({path:join(dir,'dragged-attachment.png')});
        result.drag={draftAttachment:await preview.getAttribute('title'),thumbnail:box,imageReads:reads.length,mediaReads:mediaReads.length};
        assert.equal(mediaReads.length,0);
        const sessionId=await session();
        await page.reload();
        await page.getByLabel('Allow local HTTP for development',{exact:true}).check();
        await page.locator('#reconnect').click();
        await page.waitForFunction(id=>document.querySelector('.pane.selected')?.dataset.sessionId===id,sessionId);
        await image.waitFor();
        assert.deepEqual(await image.evaluate(el=>[el.naturalWidth,el.naturalHeight]),[84,52]);
        assert.equal(mediaReads.length,1);
        await page.screenshot({path:join(dir,'restored-attachment.png')});
        result.drag.restoredMediaReads=mediaReads.length;
        await chip.getByRole('button',{name:'Remove image',exact:true}).click();
        await chip.waitFor({state:'detached'});assert.equal(result.drag.imageReads,0);
      }finally{author.off('request',watch);}
      await verifyAttachmentRail(page,page.locator('.pane.selected'),dir);
      result.attachmentRail={cards:8,duplicateContent:true,mouseWheel:true,paging:true,resize:true};
    }
    const id=await session();assert(id);
    await author.close();
    await service.restart();
    const second=JSON.parse(service.run(['run','devices','register','Transcript image reader']).stdout);
    assert.notEqual(first.id,second.id);
    const {devices}=JSON.parse(service.run(['run','devices','configuration','list']).stdout);
    assert(devices.includes(first.id));
    result.readerConfigurationAccess=devices.includes(second.id);
    assert.equal(result.readerConfigurationAccess,false);
    await writeFile(join(service.workspace,'notes.md'),'First line\nChanged since first read\nLast line');
    await writeFile(join(dirname(service.workspace),'.outside.png'),png(248,90,[61,106,170,255]));
    await writeFile(join(dirname(service.workspace),'外部 草图.png'),png(248,90,[85,102,160,255]));
    await writeFile(join(dirname(service.workspace),'linked.png'),png(248,90,[52,132,165,255]));
    await connect(second,390);
    await page.getByRole('button',{name:'Toggle navigation',exact:true}).click();
    const row=page.locator('[data-testid="conversation-row"][data-session-id="'+id+'"]').getByTestId('conversation-open');
    if(!await row.isVisible()){
      const group=page.locator('[data-testid="workspace-group"] button[aria-expanded=false]').first();
      if(await group.isVisible())await group.click();
    }
    await row.click();await inspect('second-device-reopened',248);
    const facts=await service.readSessionFacts(id);
    result.modelIntents=facts.filter(x=>x.type==='model_intent').length;
    result.mockCalls=service.provider.requests.length;
    result.browser=browser.version();assert.equal(result.modelIntents,1);assert.equal(result.mockCalls,live?0:1);
    assert(result.unreadableResponses.length>=2 && result.unreadableResponses.every(status=>status===200 || status===422),
      'both devices received a result for the unreadable image');
    assert.deepEqual(result.errors,[]);result.ok=true;
  }catch(e){result.failure=e.message;await page?.screenshot({path:join(dir,'failure.png')}).catch(()=>{});throw e;}
  finally{try{await browser?.close();}finally{await service.close();await writeFile(join(dir,'result.json'),JSON.stringify(result,null,2));}}
}
