// xterm 6.0.0 CoreBrowserTerminal.open has a precedence error when passing its
// public documentOverride to CoreBrowserService. Keep all three style owners on
// the same document; refuse an unreviewed dependency change.
import {createHash} from 'node:crypto';
import {readFileSync} from 'node:fs';
import {createRequire} from 'node:module';
import {dirname, resolve} from 'node:path';
export function xtermDocumentOverride() {
  const require=createRequire(import.meta.url);
  const manifest=require.resolve('@xterm/xterm/package.json');
  const {module}=JSON.parse(readFileSync(manifest,'utf8'));
  if(typeof module!=='string')throw new Error('xterm no longer declares its ES module');
  const entry=resolve(dirname(manifest),module);
  const normalize=id=>id.split('?')[0].replaceAll('\\','/');
  const source=readFileSync(entry,'utf8');
    if(createHash('sha256').update(source).digest('hex')!=='b336ec65a086c056d4804b3d4c2347da5663d3f23c3f25be866467bd8857ad59')throw new Error('Review the xterm document override patch for this dependency');
    const before='this._document??typeof window<"u"?window.document:null';
    if(source.split(before).length!==2)throw new Error('xterm document override patch no longer applies exactly once');
  const patched=source.replace(before,'this._document??(typeof window<"u"?window.document:null)');
  let building=false,loaded=false;
  return {name:'rsi-xterm-document-override',enforce:'pre',
    configResolved(config){building=config.command==='build'},
    buildStart(){loaded=false},
    async resolveId(source,importer,options){
      if(source!=='@xterm/xterm')return;
      const resolved=await this.resolve(source,importer,{...options,skipSelf:true});
      if(!resolved||normalize(resolved.id)!==normalize(entry))throw new Error('xterm resolved outside the reviewed document override patch');
      return resolved;
    },
    load(id){
      if(normalize(id)!==normalize(entry))return;
      loaded=true;return patched;
    },
    buildEnd(error){if(building&&!error&&!loaded)throw new Error('Production build did not load the patched xterm module')},
  };
}
