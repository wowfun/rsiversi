import { createSettingsFields } from "./settings-fields.js";
const version = (editor) => JSON.stringify(editor.version);
export function createSettingsDraft(initial, command) {
  let revision = 0,
    base,
    dirty = false,
    awaiting,
    retired = false;
  const listeners = new Set(),
    changed = () => {
      revision++;
      for (const listener of listeners) listener();
    };
  const draft = {
    current: initial,
    saving: false,
    message: "",
    fields: undefined,
    subscribe: (listener) => {
      listeners.add(listener);
      return () => listeners.delete(listener);
    },
    getSnapshot: () => revision,
    get conflict() {
      return version(draft.current) !== base;
    },
    get disabled() {
      return draft.saving || !!awaiting || !draft.current.description.writable;
    },
    get canSave() {
      return !draft.disabled && !draft.conflict && draft.fields.canSave();
    },
    get reloadVisible() {
      return draft.conflict || !!awaiting;
    },
    get awaiting() {
      return !!awaiting;
    },
    get error() {
      return (
        draft.message ||
        (awaiting
          ? "Waiting for refreshed settings. Reload settings if this does not finish."
          : draft.conflict
            ? "Settings changed while this draft was open. Copy your edits or discard them and reload before saving."
            : "")
      );
    },
    update(editor) {
      if (retired) return;
      draft.current = editor;
      if (!dirty && !draft.saving && (!awaiting || editor.ticket !== awaiting))
        draw(editor);
      else changed();
    },
    async reload() {
      if (retired || draft.saving) return;
      const ticket = draft.current.ticket;
      draft.saving = true;
      draft.message = "";
      changed();
      try {
        await command({
          action: "settings_read",
          namespace: draft.current.namespace,
        });
        if (retired) return;
        dirty = false;
        awaiting = ticket;
        if (draft.current.ticket !== ticket) draw(draft.current);
      } catch (error) {
        if (!retired) draft.message = String(error.message ?? error);
      } finally {
        draft.saving = false;
        if (!retired) changed();
      }
    },
    async save() {
      if (retired || !draft.canSave) return;
      const ticket = draft.current.ticket,
        text = draft.fields.json();
      draft.saving = true;
      draft.message = "";
      changed();
      try {
        await command({ action: "settings_save", ticket, text });
        if (retired) return;
        dirty = false;
        awaiting = ticket;
        if (draft.current.ticket !== ticket) draw(draft.current);
      } catch (error) {
        if (!retired) draft.message = String(error.message ?? error);
      } finally {
        draft.saving = false;
        if (!retired) changed();
      }
    },
    async browse() {
      try {
        await command({ action: "settings_list" });
      } catch (error) {
        if (!retired) {
          draft.message = String(error.message ?? error);
          changed();
        }
      }
    },
    retire() {
      retired = true;
      listeners.clear();
    },
  };
  function draw(editor) {
    draft.current = editor;
    base = version(editor);
    dirty = false;
    awaiting = undefined;
    draft.message = "";
    draft.fields = createSettingsFields(
      editor.description.metadata.schema,
      editor.text,
      !editor.description.writable,
      (edited) => {
        if (edited) dirty = true;
        changed();
      },
    );
    changed();
  }
  draw(initial);
  return draft;
}
