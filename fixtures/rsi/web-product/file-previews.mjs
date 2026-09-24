import {createServer} from 'node:http';
import assert from 'node:assert/strict';
import {mkdir,writeFile} from 'node:fs/promises';
import {join} from 'node:path';
import {waitUntil} from './service.mjs';
import {png} from './images.mjs';

export async function verifyFilePreviews(page,service,report,browser) {
  const directory=join(service.workspace,'previews');await mkdir(directory,{recursive:true});
  const svg='<svg xmlns="http://www.w3.org/2000/svg" width="640" height="320"><rect width="640" height="320" fill="#0b716c"/><text x="36" y="160" fill="white" font-size="40">Workspace preview</text></svg>';
  await writeFile(join(directory,'diagram.svg'),svg);
  await writeFile(join(directory,'pixel.png'),png(1,1,[11,113,108,255]));
  await writeFile(join(directory,'sample.rs'),'// Unicode 界 — source is data\nfn main() {\n    println!("preview");\n}\n');
  await writeFile(join(directory,'report.md'),'# Preview report\n\n**Unicode 界** and ~~obsolete~~.\n\n| Result | Status |\n|---|---|\n| Workspace image | Ready |\n\n```rust\nfn answer() -> u8 { 42 }\n```\n\n![Local diagram](diagram.svg)\n\n![Missing diagram](missing.png)\n\n<script>parent.markdownExecuted = true</script>\n');
  await writeFile(join(directory,'theme.css'),'@import "colors.css"; body { font-family: sans-serif; padding: 24px; } #counter { font-size: 26px; } .banner { background-image: url(diagram.svg); height: 100px; background-size: contain; }');
  await writeFile(join(directory,'colors.css'),'body { color: rgb(10, 80, 70); background: rgb(235, 247, 241); }');
  await writeFile(join(directory,'app.js'),`let n=0; document.querySelector('#counter').onclick=()=>document.querySelector('#count').textContent=String(++n); document.querySelector('#loaded').textContent='Local script loaded'; try { parent.document.body.dataset.previewEscaped='yes'; } catch { document.querySelector('#isolated').textContent='Parent isolated'; } document.querySelector('#bridge').textContent=typeof window.__TAURI_INTERNALS__; fetch('https://preview-fixture.invalid/probe').then(r=>r.text()).then(text=>document.querySelector('#network').textContent=text).catch(()=>document.querySelector('#network').textContent='Network blocked');`);
  const html='<!doctype html><html><head><meta charset="utf-8"><link rel="stylesheet" href="theme.css"></head><body><h1>Interactive file preview</h1><div class="banner"></div><p id="loaded"></p><button id="counter">Count: <span id="count">0</span></button><p id="isolated"></p><p id="bridge"></p><p id="network">Pending</p><img src="diagram.svg" width="160"><script src="app.js"></script></body></html>';
  await writeFile(join(directory,'demo.html'),html);
  await writeFile(join(directory,'missing.html'),html.replace('<h1>Interactive file preview</h1>','<h1>Missing resources preserve HTML</h1><img alt="Missing local image" src="missing.png"><link rel="stylesheet" href="missing.css"><script src="missing.js"></script>'));
  await writeFile(join(directory,'large.txt'),'x'.repeat(1024*1024+1));
  let requests=0;await page.context().route('https://preview-fixture.invalid/**',route=>{requests++;return route.fulfill({status:200,contentType:'text/plain',headers:{'access-control-allow-origin':'*'},body:'HTTPS enabled'});});
  const bomb=png(1,1,[1,2,3,255]);bomb.writeUInt32BE(100000,16);bomb.writeUInt32BE(100000,20);
  for (let index=0;index<3;index++) await writeFile(join(directory,`large-${index}.svg`),'<svg xmlns="http://www.w3.org/2000/svg" width="4096" height="4096"><rect width="4096" height="4096" fill="teal"/></svg>');
  await writeFile(join(directory,'aggregate.html'),'<h1>Must not execute</h1>'+[0,1,2].map(index=>`<img src="large-${index}.svg">`).join(''));
  await writeFile(join(directory,'oversized.png'),bomb);
  await writeFile(join(directory,'oversized.md'),'# Kept text\n\n![Oversized](oversized.png)');
  await writeFile(join(directory,'disguised.txt'),bomb);
  await writeFile(join(directory,'disguised.html'),'<img src="disguised.txt">');
  await writeFile(join(directory,'embedded-bomb.html'),`<img src="data:image/png;base64,${bomb.toString('base64')}">`);
  await writeFile(join(directory,'embedded-bomb.md'),`# Embedded text\n\n![Embedded](data:image/png;base64,${bomb.toString('base64')})`);
  await writeFile(join(directory,'embedded.html'),`<img src="data:image/png;base64,${png(1,1,[1,2,3,255]).toString('base64')}">`);
  await writeFile(join(directory,'oversized.svg'),'<svg xmlns="http://www.w3.org/2000/svg" width="100000" height="100000"/>');
  const svgRoot='<svg xmlns="http://www.w3.org/2000/svg" width="640" height="320">';
  for(const [name,body] of [
    ['use-cycle','<g id="loop"><use href="#loop"/></g><use href="#loop"/>'],
    ['smil','<set attributeName="width" to="99999"/>'],
    ['stylesheet','<style>svg {width:99999px}</style>'],
    ['too-many-nodes','<g/>'.repeat(2048)],
    ['too-deep','<g>'.repeat(32)+'</g>'.repeat(32)],
  ])await writeFile(join(directory,`${name}.svg`),svgRoot+body+'</svg>');
  await writeFile(join(directory,'embedded-raster.svg'),`<svg xmlns="http://www.w3.org/2000/svg"><image href="data:image/png;base64,${bomb.toString('base64')}"/></svg>`);
  await page.evaluate(()=>{window.previewDecodeAttempts=0;const property=Object.getOwnPropertyDescriptor(HTMLImageElement.prototype,'src');Object.defineProperty(HTMLImageElement.prototype,'src',{...property,set(value){window.previewDecodeAttempts++;property.set.call(this,value);}});});
  const before=service.provider.requests.length;
  const open=async file=>{
    if(await page.locator('#detail').isVisible())await page.getByRole('button',{name:'Close details',exact:true}).click();
    await page.getByRole('button',{name:'Workspace files',exact:true}).click();
    const card=page.locator('.ui-contribution');await card.getByRole('textbox',{name:'Workspace-relative path',exact:true}).fill(`previews/${file}`);
    await card.getByRole('button',{name:'Read file',exact:true}).click();
    return page.locator('.file-preview');
  };
  let preview=await open('sample.rs');await preview.locator('.file-code').filter({hasText:'fn main()'}).waitFor();
  assert(await preview.locator('.file-token[style]').count()>0,'code syntax colors absent');assert(await preview.locator('.file-line-number').count()>=4);
  await preview.getByRole('button',{name:'Wrap lines',exact:true}).click();assert(await preview.locator('.file-code.wrap').count());
  await page.screenshot({path:join(report,`${browser}-preview-code.png`)});
  preview=await open('report.md');await preview.locator('table').waitFor();assert.equal(await preview.locator('table tbody tr').count(),1);
  await waitUntil(()=>preview.locator('img').evaluateAll(images=>images.length===1&&images.every(image=>image.complete&&image.naturalWidth>0)),'Markdown local image');
  assert.equal(await preview.locator('script').count(),0);assert.equal(await page.evaluate(()=>window.markdownExecuted),undefined);
  assert.match(await preview.innerText(),/Missing diagram/);await page.screenshot({path:join(report,`${browser}-preview-markdown.png`)});
  await preview.getByRole('button',{name:'Source',exact:true}).click();await preview.locator('.file-code').filter({hasText:'![Local diagram](diagram.svg)'}).waitFor();
  await writeFile(join(directory,'report.md'),'# Changed on disk\n');
  await preview.getByRole('button',{name:'Preview',exact:true}).click();await preview.getByRole('heading',{name:'Preview report',exact:true}).waitFor();
  await preview.getByRole('button',{name:'Refresh',exact:true}).click();await preview.getByRole('heading',{name:'Changed on disk',exact:true}).waitFor();
  preview=await open('missing.html');
  const missingFrame=page.frameLocator('iframe.file-html');
  await missingFrame.getByRole('heading',{name:'Missing resources preserve HTML'}).waitFor();
  await missingFrame.locator('#loaded').filter({hasText:'Local script loaded'}).waitFor();
  await missingFrame.locator('#counter').click();assert.equal(await missingFrame.locator('#count').textContent(),'1');
  assert.equal(await missingFrame.getByAltText('Missing local image').getAttribute('src'),'about:blank');
  assert.equal(await preview.locator('.source-error').count(),0);
  assert.match(await preview.innerText(),/missing.png/);
  await page.screenshot({path:join(report,`${browser}-preview-missing.png`)});
  preview=await open('diagram.svg');await waitUntil(()=>preview.locator('img').evaluate(image=>image.complete&&image.naturalWidth===640),'SVG image decode');
  await preview.getByRole('button',{name:'100%',exact:true}).click();assert.equal(await preview.locator('img').evaluate(image=>image.style.width),'640px');await preview.getByRole('button',{name:'Zoom in',exact:true}).click();assert.equal(await preview.locator('img').evaluate(image=>image.style.width),'800px');await preview.getByRole('button',{name:'Fit',exact:true}).click();
  await page.screenshot({path:join(report,`${browser}-preview-image.png`)});
  preview=await open('pixel.png');await waitUntil(()=>preview.locator('img').evaluate(image=>image.complete&&image.naturalWidth===1),'PNG raster decode');
  const attempts=await page.evaluate(()=>window.previewDecodeAttempts);
  await open('oversized.png');await page.locator('.source-error').filter({hasText:'pixel limit'}).waitFor();
  assert.equal(await page.evaluate(()=>window.previewDecodeAttempts),attempts,'oversized image reached browser decode');
  await page.getByRole('button',{name:'View exact hex',exact:true}).click();
  await page.locator('.ui-text').filter({hasText:/89 50 4e 47/i}).waitFor();
  assert.equal(await page.evaluate(()=>window.previewDecodeAttempts),attempts,'hex fallback retried image decode');
  preview=await open('oversized.md');await preview.getByRole('heading',{name:'Kept text'}).waitFor();await preview.locator('.file-resource-error').filter({hasText:'pixel limit'}).waitFor();
  assert.equal(await page.evaluate(()=>window.previewDecodeAttempts),attempts,'Markdown oversized image reached browser decode');
  for(const file of ['disguised.html','embedded-bomb.html']) {
    preview=await open(file);await preview.locator('.source-error').filter({hasText:'pixel limit'}).waitFor();
    assert.equal(await page.frameLocator('iframe.file-html').locator('img').count(),0,'unsafe HTML reached document.write');
  }
  preview=await open('aggregate.html');await preview.locator('.source-error').filter({hasText:'aggregate pixel limit'}).waitFor();
  assert.equal(await page.frameLocator('iframe.file-html').locator('h1, img').count(),0,'aggregate image rejection must precede document.write');
  await page.screenshot({path:join(report,`${browser}-preview-aggregate.png`)});
  preview=await open('embedded-bomb.md');await preview.getByRole('heading',{name:'Embedded text'}).waitFor();await preview.locator('.file-resource-error').filter({hasText:'pixel limit'}).waitFor();
  for(const [file,reason] of [['oversized.svg','pixel limit'],['embedded-raster.svg','malformed']]) {await open(file);await page.locator('.source-error').filter({hasText:reason}).waitFor();}
  for(const [file,reason] of [['use-cycle','malformed'],['smil','malformed'],['stylesheet','malformed'],['too-many-nodes','node or nesting'],['too-deep','node or nesting']]) {
    await open(`${file}.svg`);await page.locator('.source-error').filter({hasText:reason}).waitFor();
  }
  assert.equal(await page.evaluate(()=>window.previewDecodeAttempts),attempts,'unsupported SVG reached browser decode');
  preview=await open('embedded.html');await waitUntil(()=>page.frameLocator('iframe.file-html').locator('img').evaluate(image=>image.complete&&image.naturalWidth===1),'bounded embedded image');
  const child=page.frames().find(frame=>frame.url().includes('/preview-local.html'));
  assert(await child.evaluate(encoded=>new Promise(resolve=>{document.addEventListener('securitypolicyviolation',event=>{if(event.effectiveDirective==='img-src'&&event.blockedURI==='data')resolve(true);},{once:true});const image=new Image();image.onload=()=>resolve(false);image.src='data:image/png;base64,'+encoded;document.body.append(image);}),png(1,1,[1,2,3,255]).toString('base64')), 'response CSP must block raw data images');
  preview=await open('demo.html');let frame=page.frameLocator('iframe.file-html');await frame.locator('#loaded').filter({hasText:'Local script loaded'}).waitFor();await frame.locator('#network').filter({hasText:'Network blocked'}).waitFor();assert.equal(requests,0);
  assert.equal(await frame.locator('#isolated').textContent(),'Parent isolated');assert.equal(await frame.locator('#bridge').textContent(),'undefined');assert.equal(await page.locator('body').getAttribute('data-preview-escaped'),null);
  assert.equal(await frame.locator('body').evaluate(body=>getComputedStyle(body).backgroundColor),'rgb(235, 247, 241)');
  await frame.locator('#counter').click();assert.equal(await frame.locator('#count').textContent(),'1');
  await waitUntil(()=>frame.locator('img').evaluate(image=>image.complete&&image.naturalWidth===640),'HTML local image');
  await page.screenshot({path:join(report,`${browser}-preview-html-local.png`)});
  await preview.getByRole('button',{name:'Enable HTTPS resources',exact:true}).click();frame=page.frameLocator('iframe.file-html');await frame.locator('#network').filter({hasText:'HTTPS enabled'}).waitFor();assert.equal(requests,1);assert.equal(await frame.locator('#count').textContent(),'0');
  await page.screenshot({path:join(report,`${browser}-preview-html-https.png`)});
  await preview.getByRole('button',{name:'Source',exact:true}).click();assert.equal(await page.locator('iframe.file-html').count(),0);await preview.locator('.file-code').filter({hasText:'<!doctype html>'}).waitFor();
  await preview.getByRole('button',{name:'Preview',exact:true}).click();await frame.locator('#network').filter({hasText:'HTTPS enabled'}).waitFor();assert.equal(requests,2);
  await open('demo.html');frame=page.frameLocator('iframe.file-html');await frame.locator('#network').filter({hasText:'Network blocked'}).waitFor();assert.equal(requests,2);
  await page.setViewportSize({width:720,height:980});await page.screenshot({path:join(report,`${browser}-preview-html-narrow.png`)});await page.setViewportSize({width:1440,height:980});
  await open('large.txt');await page.locator('.ui-contribution').filter({hasText:'Complete preview exceeds 1 MiB'}).waitFor();await page.getByRole('button',{name:'Next page',exact:true}).waitFor();
  await page.getByRole('button',{name:'Close details',exact:true}).click();assert.equal(service.provider.requests.length,before,'preview invoked a model');
  assert.equal(await page.evaluate(()=>window.previewUrls.size),0,'preview Blob URLs survive close');
  const policy=await verifyPreviewPolicy(page,service.origin);
  return {policy,predecode_bounds:true,aggregate_pixel_bound:true,formats:['code','markdown','png','svg','html'],local_resources:true,https_requests:requests,model_requests:0,version_refresh:true};
}

export async function verifyPreviewPolicy(page,origin) {
  await page.evaluate(()=>{window.policyViolations=[];window.policyListener=e=>window.policyViolations.push(e.effectiveDirective);document.addEventListener('securitypolicyviolation',window.policyListener);const f=document.createElement('iframe');f.id='policy-blocked';f.src='https://foreign-preview.invalid/probe';document.body.append(f);});
  await page.waitForFunction(()=>window.policyViolations.includes('frame-src'));
  await page.evaluate(()=>{document.querySelector('#policy-blocked').remove();document.removeEventListener('securitypolicyviolation',window.policyListener);});
  const foreign=await page.context().newPage();
  const server=createServer((_request,response)=>{response.writeHead(200,{'content-type':'text/html'});response.end('<!doctype html><html><body>Foreign ancestor</body></html>');});
  await new Promise((resolve,reject)=>{server.once('error',reject);server.listen(0,'127.0.0.1',resolve);});
  const foreignUrl=new URL(`http://127.0.0.1:${server.address().port}/ancestor`);
  const address=foreignUrl.href,asset=origin+'/preview-local.html',diagnostics=[];
  foreign.on('console',message=>diagnostics.push(message.text()));
  const insert=()=>foreign.evaluate(asset=>{window.policyReady=false;const f=document.createElement('iframe');f.id='ancestor-probe';window.addEventListener('message',e=>{if(e.source===f.contentWindow&&e.data?.type==='rsi-preview-ready')window.policyReady=true});f.src=asset;document.body.append(f);},asset);
  try {
    await foreign.goto(address);
    assert.equal(await foreign.evaluate(()=>location.origin),foreignUrl.origin);
    // Identical bytes and transport, with only frame-ancestors removed, must load.
    await foreign.route(asset,async route=>{const response=await route.fetch();const headers=response.headers();headers['content-security-policy']=headers['content-security-policy'].replace(/(?:^|;)\s*frame-ancestors\s+[^;]*/,'');await route.fulfill({response,headers});});
    await insert();await foreign.waitForFunction(()=>window.policyReady);
    await foreign.locator('#ancestor-probe').evaluate(node=>node.remove());await foreign.unroute(asset);
    const blocked=foreign.waitForEvent('requestfailed',{predicate:request=>request.url()===asset,timeout:10000});
    await insert();const request=await blocked;
    assert(!/LOCAL_NETWORK|CERT|SSL/.test(request.failure().errorText),request.failure().errorText);
    assert.equal(await foreign.evaluate(()=>window.policyReady),false);
    assert(diagnostics.some(message=>message.includes('frame-ancestors')),'ancestor rejection did not produce CSP evidence');
    return {frame_src_foreign_blocked:true,foreign_ancestor_blocked:true,permissive_ancestor_control:true,ancestor_failure:request.failure().errorText,diagnostics};
  } finally {await foreign.close();await new Promise(resolve=>server.close(resolve));}

}
