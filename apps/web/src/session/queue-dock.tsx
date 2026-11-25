import { useEffect, useId, useState } from "react";
import {DockButton, InlineEditor} from "./dock-controls.tsx";
import {
  IconCheckOutlineRegular, IconChevronDownOutlineRegular, IconChevronUpOutlineRegular,
  IconCloseOutlineRegular, IconEditOutlineRegular, IconQueueOutlineRegular,
  IconSendOutlineRegular, IconTrashOutlineRegular,
} from "../../vendor/dsh/dockkit-primitives/icons/index.tsx";
import { run } from "../bridge.ts";
import type { ProjectionActions, SessionProjections } from "./projection-types.ts";
import type { QueueEditorState } from "./queue-editor.tsx";

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
          const disabled = !owner.queueBinding || !!editor?.saving;
          return (
            <li key={slot} className="queue-row" data-slot={slot}>
              {single && <IconQueueOutlineRegular size={16}/>}
              <span hidden={!!inline} className="queue-summary">
                {item.source_kind} · {item.delivery_label} · {item.message_id}
              </span>
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
