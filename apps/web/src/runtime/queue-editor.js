// A queued replacement retains every non-text block verbatim until explicitly removed.
export function createQueueEditor({
  content,
  editable,
  submit,
  changed,
  closed,
}) {
  let sequence = 0,
    retired = false;
  const entry = (value) => ({ key: String(++sequence), value });
  const editor = {
    blocks: structuredClone(content).map(entry),
    editable,
    saving: false,
    status: "",
    edit(key, text) {
      if (!editable || retired || editor.saving) return;
      const block = editor.blocks.find((block) => block.key === key);
      if (block?.value.type === "text") {
        block.value.text = text;
        changed();
      }
    },
    remove(key) {
      if (!editable || retired || editor.saving) return;
      editor.blocks = editor.blocks.filter((block) => block.key !== key);
      changed();
    },
    addText() {
      if (!editable || retired || editor.saving) return;
      editor.blocks = [...editor.blocks, entry({ type: "text", text: "" })];
      changed();
    },
    async save() {
      if (!editable || retired || editor.saving) return;
      editor.saving = true;
      editor.status = "Saving queue edit…";
      changed();
      try {
        await submit(
          structuredClone(editor.blocks.map((block) => block.value)),
        );
        editor.close();
      } catch (error) {
        if (!retired) editor.status = error.message;
        throw error;
      } finally {
        editor.saving = false;
        if (!retired) changed();
      }
    },
    close() {
      if (!retired) {
        retired = true;
        closed();
      }
    },
  };
  return editor;
}
