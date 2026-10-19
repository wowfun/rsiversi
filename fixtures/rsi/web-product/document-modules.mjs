import {readFile} from 'node:fs/promises';
import {join} from 'node:path';

// Route only fixture-selected source files; browser imports use their real ESM graph.
export async function documentModules(page,root,origin,document,files) {
  const sources=new Map(await Promise.all(files.map(async file=>[file,await readFile(join(root,'apps/web',file),'utf8')])));
  await page.route(`${origin}/**`,route=>{
    const file=new URL(route.request().url()).pathname.slice(1);
    if(!file)return route.fulfill({contentType:'text/html',body:document});
    const body=sources.get(file);
    if(body===undefined)return route.fulfill({status:404,body:'Unknown fixture source'});
    return route.fulfill({contentType:file.endsWith('.css')?'text/css':'text/javascript',body});
  });
  await page.goto(origin+'/');
}
