import { keyboardAction } from "../../composer-actions.js";
import { externalDrafts } from "../../external-drafts.js";

// Rust owns observed history and one-shot controls. This owner retains only the
// bounded document drafts and their exact pending/unknown revisions.
export function externalPaneClass({ command, perform }) {
  const drafts = externalDrafts();
  return class ExternalPane {
    static clearDrafts() {
      drafts.clear();
    }
    kind = "external";
    revision = 0;
    listeners = new Set();
    subscribe = (listener) => {
      this.listeners.add(listener);
      return () => this.listeners.delete(listener);
    };
    getSnapshot = () => this.revision;
    invalidate() {
      this.revision++;
      for (const listener of this.listeners) listener();
    }
    constructor(index) {
      this.index = index;
      this.composerView = {
        text: "",
        inputDisabled: true,
        sendDisabled: true,
        cancelDisabled: true,
        notice: "",
      };
    }
    mount(elements) {
      Object.assign(this, elements);
      this.events = new AbortController();
      const on = (node, event, run) =>
        node.addEventListener(event, run, { signal: this.events.signal });
      on(this.input, "input", () => this.save());
      on(this.input, "keydown", (event) => {
        if (keyboardAction(event, this.submitKey ?? "enter")) {
          event.preventDefault();
          perform(() => this.submit());
        }
      });
    }
    unmount() {
      this.events?.abort();
    }
    dispose() {
      this.reset();
      this.unmount();
    }
    applyFrame() {
      const next = this.staged;
      if (!next) return;
      this.submitKey = next.submitKey;
      this.composerActions = next.composerActions;
      this.render(next.data);
    }
    changed = () => {
      if (
        this.record &&
        (this.record.revision !== this.capacityRevision ||
          !drafts.current(this.record))
      )
        this.capacity = false;
      const text = this.record?.text;
      if (this.input && text !== undefined && this.input.value !== text) {
        const { selectionStart, selectionEnd } = this.input;
        this.input.value = text;
        this.input.setSelectionRange?.(
          Math.min(selectionStart, text.length),
          Math.min(selectionEnd, text.length),
        );
      }
      this.renderComposer();
    };
    get uncertain() {
      return (
        !!this.record && this.record.unknownRevision === this.record.revision
      );
    }
    get submitting() {
      return !!this.record?.pending;
    }
    save() {
      const accepted = drafts.edit(
        this.record,
        this.input?.value ?? this.record?.text ?? "",
      );
      if (!accepted) {
        this.capacity = true;
        this.capacityRevision = this.record?.revision;
        this.changed();
      }
      return accepted;
    }
    async flush() {
      if (this.id) this.save();
    }
    reset() {
      if (this.id) this.save();
      drafts.unbind(this.record, this.changed);
      this.record = null;
      this.id = undefined;
      if (this.input) this.input.value = "";
      this.generation = undefined;
      this.data = undefined;
      this.staged = undefined;
      this.capacity = false;
      this.bindingVersion = undefined;
      this.renderComposer();
    }
    action(fields) {
      return command({
        pane: this.index,
        generation: this.generation,
        ...fields,
      });
    }
    async control(value) {
      await this.action({ action: "external_control", command: value });
    }
    async submit() {
      if (
        this.switching ||
        this.submitting ||
        !this.data?.capabilities.submit ||
        this.data?.busy ||
        !this.save() ||
        this.uncertain ||
        !this.record?.text.trim()
      )
        return;
      const attempt = drafts.begin(this.record);
      if (!attempt) return;
      try {
        await this.control({ kind: "submit", text: attempt.text });
        drafts.finish(attempt, true);
      } catch (error) {
        drafts.finish(attempt, false);
        throw error;
      }
    }
    renderComposer() {
      this.composerView = {
        text: this.record?.text ?? "",
        inputDisabled: !!this.switching || !drafts.current(this.record),
        sendDisabled: !!(
          this.switching ||
          this.submitting ||
          this.uncertain ||
          !drafts.current(this.record) ||
          this.data?.busy ||
          !this.data?.capabilities.submit ||
          !this.record?.text.trim()
        ),
        cancelDisabled: !!(
          this.data?.busy ||
          !this.data?.observed.connected ||
          this.data?.observed.snapshot.status !== "running"
        ),
        notice:
          [
            this.uncertain
              ? "The send outcome is unknown. Your text is retained. Check the conversation before writing another prompt."
              : "",
            this.capacity
              ? this.record
                ? "External draft capacity is full. Existing text is retained."
                : "External draft capacity is full. Close another pane to enable composition."
              : "",
          ]
            .filter(Boolean)
            .join(" ") ||
          this.data?.diagnostic ||
          "",
      };
      this.invalidate();
    }
    render(data) {
      if (!data?.external) return;
      const state = data.external,
        snapshot = state.observed.snapshot;
      if (
        this.generation !== data.generation ||
        this.id !== snapshot.id ||
        (this.record && !drafts.current(this.record))
      ) {
        if (this.id && drafts.current(this.record)) this.save();
        drafts.unbind(this.record, this.changed);
        this.generation = data.generation;
        this.id = snapshot.id;
        this.record = drafts.bind(this.id, this.changed);
        this.capacity = !this.record;
        this.bindingVersion = drafts.version();
        if (this.input) this.input.value = this.record?.text ?? "";
        this.switching = false;
      }
      if (!this.record && this.bindingVersion !== drafts.version()) {
        this.record = drafts.bind(this.id, this.changed);
        this.capacity = !this.record;
        this.bindingVersion = drafts.version();
        if (this.record && this.input) this.input.value = this.record.text;
      }
      this.data = state;
      this.renderComposer();
    }
  };
}
