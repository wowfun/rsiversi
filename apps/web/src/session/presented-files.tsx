import {useState} from 'react';
import {PaperGlyph} from '../../vendor/dsh/dockkit-primitives/FileGlyphs.tsx';
import {run} from '../bridge.ts';
import type {TranscriptBlock} from '../presentation-types.ts';
import type {TranscriptOwner} from './transcript.tsx';
import css from './presented-files.module.css';

type FileEntry = NonNullable<TranscriptBlock['presented']>[number];

function FileCard({entry, pane, generation, details}: {entry:FileEntry; pane:string; generation:string; details:()=>void}) {
  const {path} = entry;
  const label = path ?? entry.path_hex;
  const name = label.split('/').at(-1)!;
  const description = entry.description.replace(/\s*(?:\([^()]*\)|（[^（）]*）)\s*$/u, '').trim()
    || (path && /\.([^.]+)$/.exec(name)?.[1].toUpperCase()) || 'File';
  const open = () => {
    if (path === undefined) return details();
    window.dispatchEvent(new CustomEvent('rsi:workspace-document', {detail:{pane, generation, path, details}}));
  };
  return <button type="button" className={css.card} title={label} data-presented-file=""
    aria-label={path === undefined ? 'View file details for '+label : 'Preview '+label+' in sidebar'} onClick={open}>
    <span className={css.tile}><PaperGlyph/></span>
    <span className={css.description}><strong>{name}</strong>
      <small><span>{description}</span><span>{path === undefined ? 'View file details' : 'Preview in sidebar'}</span></small>
    </span>
  </button>;
}

export function PresentedFiles({process, blocks, pane, generation, owner}: {
  process:string[]; blocks:Map<string,TranscriptBlock>; pane:string; generation:string; owner:TranscriptOwner;
}) {
  const [expanded, setExpanded] = useState(false);
  const declarations = process.flatMap(key => {
    const block = blocks.get(key);
    return block?.presented && block.tool?.result ? [{key, entries:block.presented, seq:block.tool.result.seq}] : [];
  });
  if (!declarations.length) return null;
  declarations.sort((a,b) => a.seq.length-b.seq.length || (a.seq < b.seq ? -1 : a.seq > b.seq ? 1 : 0));
  const files = new Map<string,{key:string; entry:FileEntry}>();
  for (const {key, entries} of declarations)
    for (const entry of entries) files.set(entry.path ?? '\0'+entry.path_hex, {key, entry});
  const rows = Array.from(files);
  return <div className={css.group}><div className={css.grid} data-single={rows.length===1}>
    {(expanded ? rows : rows.slice(0,4)).map(([id,{entry,key}]) => <FileCard key={id}
      entry={entry} pane={pane} generation={generation} details={()=>void run(()=>owner.action('ui_block',{key}))}/>)}
  </div>
    {rows.length>4 && <button type="button" className={css.toggle} aria-expanded={expanded}
      aria-label={expanded ? 'Collapse delivered files' : 'Show all '+rows.length+' delivered files'}
      onClick={()=>setExpanded(value=>!value)}>{expanded ? 'Collapse' : 'All '+rows.length+' files'}</button>}
  </div>;
}
