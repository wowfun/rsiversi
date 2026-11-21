import {readFile} from 'node:fs/promises';
import {join} from 'node:path';

export async function serveDraftModules(context, root, port) {
  const modules = ['drafts.js','draft-databases.js','starting-draft.js','storage-namespace.js'];
  const sources = new Map(await Promise.all(modules.map(async name =>
    ['/'+name, await readFile(join(root,'apps/web',name),'utf8')])));
  await context.route(`http://localhost:${port}/**`, route => {
    const path = new URL(route.request().url()).pathname;
    return path === '/' ? route.fulfill({contentType:'text/html',body:'<!doctype html>'}) :
      route.fulfill({status:sources.has(path) ? 200 : 404, contentType:'text/javascript',body:sources.get(path) ?? ''});
  });
}
