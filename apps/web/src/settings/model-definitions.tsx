import {useState, type Dispatch, type SetStateAction} from 'react';
import {Button} from '../button.tsx';
import {IconChevronDownOutlineRegular,IconChevronRightOutlineRegular,IconTrashOutlineRegular,IconPlusOutlineRegular} from '../../vendor/dsh/dockkit-primitives/icons/index.tsx';
import {newRow,type Row} from './provider-draft.ts';
import css from './models.module.css';
export {newRow,type Row,type Limits} from './provider-draft.ts';
export function ModelDefinitions({rows,setRows}:{rows:Row[];setRows:Dispatch<SetStateAction<Row[]>>}) {
  const [expanded,setExpanded]=useState<Set<string>>(()=>new Set());
  return <>
    {!rows.length&&<p className={css.empty}>No models configured. Fetch available models or add a model ID.</p>}
    {rows.map((row,index)=>{
      const open=expanded.has(row.key);
      const update=(change:Partial<Row>)=>setRows(previous=>previous.map(item=>item.key===row.key?{...item,...change}:item));
      return <div className={css.model} key={row.key}>
        <div className={css.modelHeading}><input aria-label={`Model ID ${index+1}`} placeholder="Model ID" value={row.name} maxLength={128} onChange={event=>update({name:event.target.value})}/>
          <button type="button" aria-label={`Model settings ${index+1}`} aria-expanded={open} onClick={()=>setExpanded(previous=>{const next=new Set(previous);next.has(row.key)?next.delete(row.key):next.add(row.key);return next;})}>{open?<IconChevronDownOutlineRegular/>:<IconChevronRightOutlineRegular/>}</button>
          <button type="button" aria-label={`Remove model ${index+1}`} onClick={()=>setRows(previous=>previous.filter(item=>item.key!==row.key))}><IconTrashOutlineRegular/></button></div>
        {open&&<div className={css.limits}>{([['context_window_tokens','Context window'],['max_output_reserve_tokens','Max output tokens'],['default_output_reserve_tokens','Default output tokens']] as const).map(([key,label])=><label key={key}>{label}<input aria-label={`${label} ${index+1}`} inputMode="numeric" value={row.limits[key]} onChange={event=>update({limits:{...row.limits,[key]:event.target.value}})}/></label>)}</div>}
      </div>;
    })}
    <Button size="sm" variant="outline" onClick={()=>setRows(previous=>[...previous,newRow()])}><IconPlusOutlineRegular size={14}/>Add model</Button>
  </>;
}
