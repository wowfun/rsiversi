import { memo, useEffect, useId, useState } from "react";
import {DockButton, InlineEditor} from "./dock-controls.tsx";
import {
  IconCheckOutlineRegular, IconChevronDownOutlineRegular, IconChevronUpOutlineRegular,
  IconCloseOutlineRegular, IconEditOutlineRegular, IconQueueOutlineRegular,
  IconSendOutlineRegular, IconTrashOutlineRegular,
} from "../../vendor/dsh/dockkit-primitives/icons/index.tsx";
import { run } from "../bridge.ts";
import {fileSizeText} from "../../vendor/dsh/primitives/file-size.ts";
import {PaperGlyph} from "../../vendor/dsh/dockkit-primitives/FileGlyphs.tsx";
import type { ProjectionActions, QueuedBlock, SessionProjections } from "./projection-types.ts";
import type { QueueEditorState } from "./queue-editor.tsx";

function QueueThumb({owner,slot,id,block,index,generation}:{
  owner:ProjectionActions; slot:string; id:string; block:Extract<QueuedBlock,{type:"image"}>; index:number; generation:string;
}) {
  const [preview,setPreview] = useState<{url:string; release():void}>();
  useEffect(() => {
    let live = true;
    let interest: ReturnType<ProjectionActions["acquireQueueImage"]>;
    try {interest = owner.acquireQueueImage(block.media,generation,slot,id,index);}
    catch {return;}
    void interest.result.then(url => {
      if (live && url) setPreview({url, release:() => interest.release()});
    }, () => {});
    return () => {live = false; interest.release();};
  }, [owner,generation,slot,id,block,index]);
  const label = block.presentation?.name ?? "Queued image";
  return preview ? <img className="queue-thumb" src={preview.url} alt={label}
    onLoad={preview.release} onError={() => {preview.release(); setPreview(undefined);}}/> :
    <span className="queue-thumb" role="img" aria-label={label}/>;
}

const QueueSummary = memo(function QueueSummary({slot,id,label,owner,ready,generation}: {
  slot:string; id:string; label:string; owner:ProjectionActions; ready:boolean; generation?:string;
}) {
  const [content,setContent] = useState<QueuedBlock[]>();
  const [failed,setFailed] = useState(false);
  useEffect(() => {
    if (!ready || content) return;
    let live = true;
    void owner.readQueueContent(slot,id).then(
      blocks => {if (live && blocks) setContent(blocks);},
      () => {if (live) setFailed(true);},
    );
    return () => {live = false;};
  }, [owner,ready,slot,id,content]);
  if (!content) return <span className="queue-summary">{failed ? "Preview unavailable" : label}</span>;
  const text = content.flatMap(block => block.type === "text" ? [block.text] :
    block.type === "reference" ? [block.reference.preview] : []).join(" ").replace(/\s+/g," ").trim();
  const attachments = content.flatMap((block,index) => {
    if (block.type === "image" && generation) return [<QueueThumb key={index} {...{owner,slot,id,block,index,generation}}/>];
    if (block.type === "file") return [
      <span key={index} className="queue-file" title={block.file.name}>
        <PaperGlyph/><span>{block.file.name}</span><small>{fileSizeText(block.file.bytes)}</small>
      </span>,
    ];
    return [];
  });
  return <>
    {attachments.length>0 && <span className="queue-attachments">{attachments}</span>}
    <span className="queue-summary">{text}</span>
  </>;
});

function InlineEdit({ editor }: { editor: QueueEditorState }) {
  const block = editor.blocks[0]!;
  const text = block.value.type === "text" ? block.value.text : "";
  const canSave = !editor.saving && !!text.trim();
  useEffect(() => () => editor.restoreFocus(), [editor]);
  function save() {
    if (canSave) void run(() => editor.save());
  }
  return <>
    <InlineEditor label="Edit queued message" value={text} pending={editor.saving} onChange={value=>editor.edit(block.key,value)}
      onSave={save} onCancel={()=>editor.close()}/>
    <DockButton label="Save queued message" disabled={!canSave} onClick={save}><IconCheckOutlineRegular size={14}/></DockButton>
    <DockButton label="Cancel editing" disabled={editor.saving} onClick={() => editor.close()}><IconCloseOutlineRegular size={14}/></DockButton>
    {editor.status && <span role="status">{editor.status}</span>}
  </>;
}

export function PendingInputs({ items, owner, editor }: {
  items: NonNullable<SessionProjections["queue"]>;
  owner: ProjectionActions;
  editor?: QueueEditorState;
}) {
  const [expanded, setExpanded] = useState(false);
  const listId = useId();
  const single = items.length === 1;
  const visible = single || expanded || !!editor;
  const ready = owner.queueBinding?.items === items && owner.queueBinding.generation === owner.generation;
  return (
    <section className="queue-list" aria-label="Pending inputs">
      {!single && (
        <button type="button" className="queue-heading" aria-controls={listId}
          aria-expanded={visible} disabled={!!editor} onClick={() => setExpanded(!expanded)}>
          <IconQueueOutlineRegular size={16} aria-hidden/>
          {items.length} queued messages
          {visible ? <IconChevronDownOutlineRegular size={14}/> : <IconChevronUpOutlineRegular size={14}/>}
        </button>
      )}
      <ul id={listId} hidden={!visible}>
        {visible && items.map(item => {
          const slot = item.queue_slot.id;
          const inline = editor?.inlineSlot === slot ? editor : undefined;
          const action = (name: string) => void run(() => owner.queueAction(slot, name));
          const read = item.editable ? "Edit queued message" : "View queued message";
          const disabled = !ready || !!editor?.saving;
          return (
            <li key={slot} className="queue-row" data-slot={slot}>
              {single && <IconQueueOutlineRegular size={16}/>}
              {!inline && <QueueSummary key={owner.generation + ":" + item.message_id} id={item.message_id}
                label={item.delivery_label} generation={owner.generation} {...{slot,owner,ready}}/>}
              {inline && <InlineEdit editor={inline}/>}
              <div className="queue-actions" hidden={!!inline}>
                <DockButton label={read} disabled={disabled} onClick={() => action("read")}>
                  {item.editable ? <IconEditOutlineRegular size={14}/> : item.read_label}
                </DockButton>
                {item.withdraw_label && <DockButton label="Remove queued message" disabled={disabled}
                  onClick={() => action("withdraw")}><IconTrashOutlineRegular size={14}/></DockButton>}
                {item.convert_label && <DockButton label={item.convert_label} disabled={disabled}
                  onClick={() => action("convert_to_steer")}><IconSendOutlineRegular size={14}/></DockButton>}
              </div>
            </li>
          );
        })}
      </ul>
    </section>
  );
}
