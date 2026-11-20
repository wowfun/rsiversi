import {useRef, useState} from 'react';
import {createPortal} from 'react-dom';
import {IconCloseOutlineRegular, IconDislikeFillRegular, IconDislikeOutlineRegular,
  IconLikeFillRegular, IconLikeOutlineRegular} from '../../vendor/dsh/dockkit-primitives/icons/index.tsx';
import {input} from '../bridge.ts';
import {Modal} from '../presentation.tsx';
import {Toast} from '../../vendor/dsh/dockkit-primitives/Toast.tsx';
import css from './message-feedback.module.css';

const categories = {
  'task-result':'Task result',
  'instruction-following':'Instruction understanding and following',
  'product-interaction':'Product features and interaction',
  'service-stability':'Stability and speed',
  'resource-cost':'Resource usage and cost',
  'security-privacy-permission':'Security, privacy, and permissions',
  'other':'Other',
};
type Rating = 'positive' | 'negative';
type Saved = {version:string;rating:Rating} | null;
type Draft = {rating:Rating;category:string|null;text:string};

export function MessageFeedback({pane,generation,message}: {pane:string;generation:string;message:string}) {
  const [current,setCurrent] = useState<Saved>();
  const [draft,setDraft] = useState<Draft>();
  const [busy,setBusy] = useState(false);
  const [failure,setFailure] = useState('');
  const [thanks,setThanks] = useState(0);
  const loading = useRef<Promise<Saved>>();
  const row = useRef<HTMLButtonElement>(null);
  const request = (action:unknown) => input.messageFeedback(pane,generation,message,action);

  function ensure():Promise<Saved> {
    if (current !== undefined) return Promise.resolve(current);
    return loading.current ??= request({kind:'read'}).then(({current: saved}) => {
      setCurrent(saved);
      setFailure('');
      return saved;
    }).finally(() => { loading.current = undefined; });
  }
  function seed() { void ensure().catch(() => setFailure('Could not load feedback')); }

  async function save(head:Saved, next:Draft|null) {
    const text = next?.text.trim();
    if (text && new TextEncoder().encode(text).length > 8192) {
      setFailure('The description is too long. Shorten it and submit again.');
      return;
    }
    const value = next && {rating:next.rating, ...(next.category && {category:next.category}), ...(text && {note:text})};
    const result = await request({kind:'change',version:head?.version ?? null,value});
    if (!row.current) return;
    setCurrent(result.current);
    if (result.conflict) {
      setFailure('This feedback changed elsewhere. The latest state is shown.');
      return;
    }
    setDraft(undefined);
    setThanks(n => next ? n + 1 : 0);
  }
  async function choose(rating:Rating) {
    if (busy) return;
    setFailure('');
    try {
      let head = current;
      if (head === undefined) {
        setBusy(true);
        head = await ensure();
        if (!row.current) return;
      }
      if (head?.rating === rating) {
        setBusy(true);
        await save(head,null);
      } else setDraft({rating, category:null, text:''});
    } catch { setFailure('Could not load or update feedback'); }
    finally { setBusy(false); }
  }
  async function submit() {
    if (busy || !draft || current === undefined) return;
    setBusy(true);
    setFailure('');
    try { await save(current,draft); }
    catch { setFailure('Could not save feedback'); }
    finally { setBusy(false); }
  }
  const dismiss = () => { if (!busy) {setDraft(undefined);setFailure('');} };
  return <>
    {(['positive','negative'] as const).map(rating => {
      const active = current?.rating === rating;
      const label = active ? 'Remove rating' : rating === 'positive' ? 'Good response' : 'Bad response';
      const Icon = rating === 'positive' ? (active ? IconLikeFillRegular : IconLikeOutlineRegular)
        : (active ? IconDislikeFillRegular : IconDislikeOutlineRegular);
      return <button key={rating} ref={rating === 'positive' ? row : undefined} type="button" title={label}
        aria-label={label} aria-pressed={active} aria-disabled={busy || undefined} onPointerEnter={seed} onFocus={seed}
        onClick={() => void choose(rating)}><Icon size={16}/></button>;
    })}
    {failure && !draft && <span role="status" className={css.failure}>{failure}</span>}
    {thanks > 0 && !draft && <Toast key={thanks} text="Thanks for your feedback" tone="success"
      anchor={row.current?.closest('.pane')?.querySelector<HTMLElement>('.composer')} onDone={() => setThanks(0)}/>}
    {draft && createPortal(<Modal className={css.dialog} label="Submit feedback" close={dismiss} lightDismiss>
      <header><h2>Submit feedback</h2><button type="button" aria-label="Close" onClick={dismiss}><IconCloseOutlineRegular size={16}/></button></header>
      <div className={css.categories} role="group" aria-label="Feedback category">
        {Object.entries(categories).map(([id,label]) => <button key={id} type="button" disabled={busy}
          aria-pressed={draft.category === id} className={draft.category === id ? css.chip+' '+css.chipActive : css.chip}
          onClick={() => setDraft({...draft,category:draft.category === id ? null : id})}>{label}</button>)}
      </div>
      <textarea aria-label="Feedback details" className={css.detail} placeholder="Add details about this response."
        value={draft.text} readOnly={busy} onChange={e => setDraft({...draft,text:e.target.value})}/>
      {failure && <p role="alert" className={css.failure}>{failure}</p>}
      <button className={css.submit} type="button" disabled={busy} onClick={() => void submit()}>{busy ? 'Submitting…' : 'Submit'}</button>
    </Modal>,document.body)}
  </>;
}
