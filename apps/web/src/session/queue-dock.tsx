import { useEffect, useId, useState, type ReactNode } from "react";
import { Tooltip } from "../../vendor/dsh/dockkit-primitives/Tooltip.tsx";
import {
  IconCheckOutlineRegular, IconChevronDownOutlineRegular, IconChevronUpOutlineRegular,
  IconCloseOutlineRegular, IconEditOutlineRegular, IconQueueOutlineRegular,
  IconSendOutlineRegular, IconTrashOutlineRegular,
} from "../../vendor/dsh/dockkit-primitives/icons/index.tsx";
import { run } from "../bridge.ts";
import type { ProjectionActions, SessionProjections } from "./projection-types.ts";
import type { QueueEditorState } from "./queue-editor.tsx";

function QueueButton({ label, disabled, onClick, children }: {
  label: string;
  disabled: boolean;
  onClick(): void;
  children: ReactNode;
}) {
  return <Tooltip label={label} portal side="bottom" delayMs={500} disabled={disabled}>
    <button type="button" aria-label={label} disabled={disabled} onClick={onClick}>{children}</button>
  </Tooltip>;
}

function InlineEdit({ editor }: { editor: QueueEditorState }) {
  const block = editor.blocks[0]!;
  const text = block.value.type === "text" ? block.value.text : "";
  const canSave = !editor.saving && !!text.trim();
  useEffect(() => () => editor.restoreFocus(), [editor]);
  function save() {
    if (canSave) void run(() => editor.save());
  }
  return <>
    <textarea autoFocus rows={1} aria-label="Edit queued message" value={text}
      readOnly={editor.saving} onChange={event => editor.edit(block.key, event.currentTarget.value)}
      onKeyDown={event => {
        if (event.nativeEvent.isComposing || event.nativeEvent.keyCode === 229) return;
        if (event.key === "Enter" && !event.shiftKey) {
          event.preventDefault();
          save();
        } else if (event.key === "Escape" && !editor.saving) {
          event.preventDefault();
          editor.close();
        }
      }}/>
    <QueueButton label="Save queued message" disabled={!canSave} onClick={save}><IconCheckOutlineRegular size={14}/></QueueButton>
    <QueueButton label="Cancel editing" disabled={editor.saving} onClick={() => editor.close()}><IconCloseOutlineRegular size={14}/></QueueButton>
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
                <QueueButton label={read} disabled={disabled} onClick={() => action("read")}>
                  {item.editable ? <IconEditOutlineRegular size={14}/> : item.read_label}
                </QueueButton>
                {item.withdraw_label && <QueueButton label="Remove queued message" disabled={disabled}
                  onClick={() => action("withdraw")}><IconTrashOutlineRegular size={14}/></QueueButton>}
                {item.convert_label && <QueueButton label={item.convert_label} disabled={disabled}
                  onClick={() => action("convert_to_steer")}><IconSendOutlineRegular size={14}/></QueueButton>}
              </div>
            </li>
          );
        })}
      </ul>
    </section>
  );
}
