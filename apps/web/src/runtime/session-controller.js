import {bindStarting, startingWorkspace, startConversation, prepareComposer} from './starting-conversation.js';
import {accessKey, readAccess, saveAccess} from '../../composer-access.js';
import {captureOf, captureKey, canCaptureReference} from '../../drafts.js';
import {
  exportSession,
  submit,
  reconcile,
  executePending,
  executeDocumentCommand,
  submitQueue,
  upload,
} from "./session-submissions.js";
import {
  showFilePicker,
  showHistoryPicker,
  showReferencePicker,
} from "./session-pickers.js";
import {
  completionToken,
  updateCompletions,
  closeCompletions,
  completionEntries,
  pickCompletion,
  completionKey,
  renderCompletions,
} from "./session-completions.js";
import { mount, unmount } from "./session-elements.js";
import { restoreDrafts, recreateDraft } from "./session-recovery.js";
import { createQueueEditor } from "./queue-editor.js";
// The Session lifetime and commands use live owner ports across asynchronous work.
export function createPaneClass(context) {
  return class Pane {
    constructor(index) {
      this.index = index;
      this.blocks = new Map();
      this.visibleSequence = 0n;
      this.generation = undefined;
      this.editors = new Map();
      this.submitKey = "enter";
      this.images = [];
      this.revision = 0;
      this.listeners = new Set();
      this.subscribe = (listener) => {
        this.listeners.add(listener);
        return () => this.listeners.delete(listener);
      };
      this.getSnapshot = () => this.revision;
    }
    invalidate() {
      this.revision++;
      for (const listener of this.listeners) listener();
    }
    bindActions(stop, queue) {
      this.stopBinding = stop;
      this.queueBinding = queue;
      this.invalidate();
    }
    setReadingSummary(value) {
      if (this.readingSummary !== value) {
        this.readingSummary = value;
        this.invalidate();
      }
    }
    focusInput() {
      this.input.focus();
    }
    selectCommand(name) {
      this.input.value = `/${name} `;
      this.edit(this.input.value);
      this.input.focus();
    }
    mount(elements) {
      return mount(this, context, elements);
    }
    unmount() {
      return unmount(this, context);
    }
    dispose() {
      this.reset();
      this.unmount();
    }
    applyFrame() {
      if (!this.staged) return;
      const { data, submitKey, composerActions } = this.staged;
      this.submitKey = submitKey;
      this.composerActions = composerActions;
      this.render(data);
    }
    async action(action, fields = {}) {
      if (!this.generation) throw new Error("Open a conversation first");
      const generation = this.generation;
      if (action === "document_resource") {
        const connection = context.connection;
        const response = await context.call("read_session_resource", JSON.stringify({pane:this.index,generation,request:fields.request}));
        if (this.generation !== generation || context.connection !== connection || connection?.closing) throw new Error("Resource Session was replaced");
        return JSON.parse(response);
      }
      if (action === "document_command") return executeDocumentCommand(this, context, fields.name, fields.input);
      if (action === "fork_conversation") {
        if (this.switching || this.submitting || this.uploading) throw new Error("Wait for this conversation's current operation");
        this.switching = true;
        this.renderComposer();
        try {
          await this.flush();
          if (this.generation !== generation) throw new Error("Conversation changed before fork preparation");
          return await context.command({action, pane:this.index, generation, ...fields});
        } finally {
          if (this.generation === generation) { this.switching = false; this.renderComposer(); }
        }
      }
      return context.command({
        action,
        pane: this.index,
        generation,
        ...fields,
      });
    }
    scheduleVisible() {
      if (this.visibleFrame) return;
      this.visibleFrame = requestAnimationFrame(() => {
        this.visibleFrame = undefined;
        this.syncVisible();
      });
    }
    syncVisible() {
      if (
        !this.generation ||
        !this.node.isConnected ||
        this.switching ||
        context.connection?.closing
      )
        return;
      if (this.uiCards && !this.visibleBlocks.ready) return;
      const entries = this.uiCards ? this.visibleBlocks.last(4) : [];
      const key = JSON.stringify([
        this.generation,
        this.historical,
        this.uiRevision,
        entries.map(([key, entry]) => [key, entry.sourceKey]),
      ]);
      if (key === this.visibleKey) return;
      this.visibleKey = key;
      this.visiblePending = {
        generation: this.generation,
        sequence: (++this.visibleSequence).toString(),
        keys: entries.map(([key]) => key),
      };
      if (!this.visibleRunning) void this.sendVisible();
    }
    async sendVisible() {
      const owner = context.connection;
      this.visibleRunning = true;
      try {
        while (
          this.visiblePending &&
          !this.switching &&
          !context.connection?.closing
        ) {
          const request = this.visiblePending;
          this.visiblePending = undefined;
          for (let attempt = 0; attempt < 2; attempt++) {
            try {
              await owner.request(
                "command",
                JSON.stringify({
                  action: "ui_visible",
                  pane: this.index,
                  ...request,
                }),
              );
              break;
            } catch (error) {
              if (
                context.connection !== owner ||
                this.generation !== request.generation ||
                this.switching ||
                owner.closing
              )
                break;
              if (attempt === 1) {
                if (this.visibleSequence.toString() === request.sequence)
                  this.visibleKey = undefined;
                context.notify(error.message);
              }
              else await new Promise((resolve) => setTimeout(resolve, 100));
            }
          }
        }
      } finally {
        this.visibleRunning = false;
      }
    }
    renderInline(cards) {
      const owner = context.connection;
      for (const [key, entry] of this.blocks) {
        const card = cards?.[key];
        if (!card?.model || !card.ticket) continue;
        context.rendererSlots.push({
          key: `inline-${this.index}-${card.binding.epoch}`,
          surface: "pane",
          root: entry.inline,
          binding: JSON.stringify(card.binding),
          snapshot: {
            revision: card.ticket,
            model: card.model,
            busy: card.busy,
            error: card.error,
          },
          host: {
            invoke(action, input) {
              return owner.request(
                "command",
                JSON.stringify({
                  action: "ui_invoke",
                  ticket: card.ticket,
                  name: action,
                  input,
                }),
              );
            },
            source(name, offset, maximum) {
              return owner.request(
                "ui_source",
                JSON.stringify({ ticket: card.ticket, name, offset, maximum }),
              );
            },
          },
        });
      }
    }
    edit(
      text,
      images = this.editor?.images ?? [],
      references = this.editor?.references ?? [],
    ) {
      if (!this.generation && this.starting) { this.starting.edit(text); return; }
      if (!this.editor) throw new Error("The saved draft is still loading");
      this.editor.edit(text, images, references, textBytes => {
        const total = [...this.editors.values()].reduce(
          (sum, editor) => sum + (editor === this.editor ? textBytes : editor.textBytes),
          0,
        );
        if (total > 2 * 1024 * 1024)
          throw new Error("Local drafts exceed this pane's 2 MiB limit");
      });
    }
    async bindDraft(data) {
      const generation = this.generation,
        current = context.connection;
      this.editor = undefined;
      if (!data || data.protected || !current?.drafts) return;
      const store = current.drafts,
        key = JSON.stringify(store.key(this.index, data.session));
      let editor = this.editors.get(key);
      if (!editor?.dirty && !editor?.failure) {
        if (!editor && this.editors.size >= 64) {
          for (const [key, entry] of this.editors)
            if (!entry.dirty && !entry.failure && !entry.saving)
              this.editors.delete(key);
          if (this.editors.size >= 64)
            throw new Error(
              "Local draft capacity is full; save or clear an unsaved draft first",
            );
        }
        let record, failure;
        try {
          record = await store.ensure(
            this.index,
            data.session,
            data.header,
            data.creation,
          );
        } catch (error) {
          failure = error;
          record =
            editor?.record ??
            store.blank(this.index, data.session, data.header, data.creation);
        }
        editor = new context.DraftEditor(store, record);
        editor.failure = failure;
        this.editors.set(key, editor);
      }
      if (
        this.generation !== generation ||
        context.connection !== current ||
        current.closing
      )
        return;
      this.editor = editor;
      this.bindingError =
        editor.record.header !== data.header
          ? "Saved Session Header changed. The draft is retained and cannot be sent to this Session."
          : undefined;
      editor.changed = () => {
        if (this.editor === editor) this.renderComposer();
        void this.drainReferenceReleases(editor, generation).catch(() => {});
      };
      void this.drainReferenceReleases(editor, generation, true).catch(() => {});
      this.renderComposer();
      if (
        editor.record.pending &&
        editor.record.pending.phase !== "prepared" &&
        !editor.failure &&
        !this.bindingError
      ) {
        void this.reconcile("query").catch((error) =>
          context.notify(error.message),
        );
      }
    }
    async selectWorkspace(id) {
      if (this.submitting || this.switching || this.uploading) throw new Error("Wait for this conversation’s current operation");
      if (!context.view?.catalog.workspaces.some(entry => entry.id === id)) throw new Error("Workspace is unavailable");
      if (this.generation) { await this.flush(); return context.command({action:"create", pane:this.index, workspace:id}); }
      this.starting?.edit(this.starting.text, id);
    }
    startConversation() { return startConversation(this, context); }
    prepareComposer() { return prepareComposer(this, context); }
    setAccess(mode) {
      if (!this.editor) throw new Error('Open a conversation before changing access');
      if (this.submitting || this.switching || this.editor.record.pending) throw new Error('Finish the pending request before changing access');
      const key = accessKey(context.connection.drafts.namespace, this.staged.data.session);
      saveAccess(key, mode);
      this.accessPreference = {key, mode};
      this.renderComposer();
    }
    async flush(all = false) {
      await this.startBinding;
      await this.starting?.flush();
      let editors = all ? [...this.editors.values()] : undefined;
      await this.binding;
      editors ??= this.editor ? [this.editor] : [];
      const results = await Promise.allSettled(
        editors.map((editor) => editor.flush()),
      );
      const failure = results.find((result) => result.status === "rejected");
      if (failure) throw failure.reason;
    }
    referenceState(editor) {
      const states = (this.referenceStates ??= new WeakMap());
      if (!states.has(editor)) states.set(editor, {releasing:false, holdCleanup:0});
      return states.get(editor);
    }
    ownsReference(editor, capture) {
      const key = captureKey(capture), owns = item => captureKey(item) === key;
      return editor.references.some(item => owns(captureOf(item))) ||
        editor.record?.references?.some(item => owns(captureOf(item))) ||
        editor.record?.pending?.captures.some(owns) ||
        this.referenceState(editor).preparing?.some(owns);
    }
    cleanupKey(editor, capture) {
      return JSON.stringify([editor.record?.key?.slice(0, 2), captureKey(capture)]);
    }
    releaseReference(reference, editor, generation) {
      const capture = reference && captureOf(reference);
      if (
        !capture ||
        this.ownsReference(editor, capture)
      )
        return Promise.resolve(false);
      return this.referenceCleanupOwner(reference, editor, generation).retry();
    }
    referenceCleanupOwner(reference, editor, generation) {
      const capture = captureOf(reference);
      const cleanups = (this.referenceCleanup ??= new Map());
      const id = this.cleanupKey(editor, capture);
      if (cleanups.has(id)) {
        const cleanup = cleanups.get(id);
        cleanup.editor = new WeakRef(editor);
        cleanup.generation = generation;
        return cleanup;
      }
      const namespace = editor.record?.key
        ? JSON.stringify(editor.record.key.slice(0, 2))
        : undefined;
      const store = editor.store, key = editor.record?.key;
      let pending;
      const cleanup = {
        message: "",
        namespace,
        editor: new WeakRef(editor),
        generation,
        resolveEditor: async () => {
          const retained = cleanup.editor.deref();
          if (retained || !store || !key) return retained;
          const record = await store.get(key[2], key[3]);
          if (!record) return;
          const recovered = new context.DraftEditor(store, record);
          cleanup.editor = new WeakRef(recovered);
          return recovered;
        },
        retry: () => {
          if (pending) return pending;
          pending = Promise.resolve()
            .then(async () => {
              const connection = context.connection;
              if (
                namespace &&
                (connection?.closing ||
                  JSON.stringify(connection?.drafts?.namespace) !==
                    namespace)
              )
                throw new Error(
                  "Reconnect to the original endpoint and device to release this capture",
                );
              const owner = cleanup.editor.deref() ?? await cleanup.resolveEditor();
              if (context.connection !== connection || connection?.closing)
                throw new Error("Reconnect to the original endpoint and device to release this capture");
              if (owner && this.ownsReference(owner, capture))
                throw new Error("This reference is still owned by a draft or request");
              const release = context.boundCall(connection);
              return release(
                "reference_input",
                JSON.stringify({
                  pane: this.index,
                  generation: cleanup.generation ?? "",
                  operation: {
                    kind: "release",
                    target: reference.metadata.target,
                    snapshot: reference.snapshot,
                  },
                }),
              );
            })
            .then(() => {
              cleanups.delete(id);
              return true;
            })
            .catch((error) => {
              cleanup.message = `Reference cleanup failed: ${error.message}`;
              throw new Error(cleanup.message);
            })
            .finally(() => {
              pending = undefined;
              this.renderComposer();
            });
          return pending;
        },
      };
      cleanups.set(id, cleanup);
      return cleanup;
    }
    async drainReferenceReleases(editor, generation, retryFailed = false) {
      const state = this.referenceState(editor);
      if (retryFailed) await editor.flush();
      if (state.releasing) {
        if (retryFailed) state.retryRequested = true;
        return state.releasing;
      }
      if (editor.failure || editor.dirty || editor.saving || state.preparing || state.holdCleanup) {
        if (retryFailed) throw editor.failure ?? new Error("Wait for draft saving or reference preparation to finish, then retry cleanup");
        return;
      }
      const attempted = new Set();
      const next = () => editor.record.releases.find(capture =>
        !attempted.has(captureKey(capture)) &&
        !this.ownsReference(editor, capture) &&
        (retryFailed || !this.referenceCleanup?.get(this.cleanupKey(editor, capture))?.message));
      let capture = next();
      if (!capture) return;
      state.releasing = Promise.resolve().then(async () => {
        let failure;
        for (;;) {
          if (!capture && state.retryRequested) {
            state.retryRequested = false;
            retryFailed = true;
            attempted.clear();
            failure = undefined;
            capture = next();
          }
          if (!capture) break;
          attempted.add(captureKey(capture));
          try {
            const released = await this.releaseReference({metadata: {target: capture.target}, snapshot: capture.snapshot}, editor, generation);
            if (released) await editor.update(record => editor.store.acknowledgeRelease(record, capture));
          } catch (error) {
            failure ??= error;
            if (editor.failure) break;
          }
          capture = next();
        }
        if (failure) throw failure;
      }).finally(() => { state.releasing = undefined; state.retryRequested = false; });
      return state.releasing;
    }
    renderComposer() {
      const starting = !this.generation ? this.starting : undefined;
      const session = this.staged?.data?.session;
      const namespace = context.connection?.drafts?.namespace;
      if (session && namespace) {
        const key = accessKey(namespace, session);
        if (this.accessPreference?.key !== key) {
          try { this.accessPreference = {key, mode: readAccess(key)}; }
          catch (error) { this.accessPreference = {key}; context.notify(error.message); }
        }
      }
      const editor = this.editor,
        pending = editor?.record.pending;
      const sendDisabled =
        !editor ||
        !!editor.failure ||
        editor.transferring ||
        editor.resolving ||
        !!this.bindingError ||
        this.submitting ||
        this.uploading ||
        this.switching ||
        (!pending && (!this.composerActions || (!editor.text.trim() && !editor.images.length && !editor.references.length)));
      const fileDisabled =
        !editor || !!this.bindingError || editor.transferring || editor.resolving || this.switching;
      this.images = editor?.images ?? [];
      this.composerView = {
        completionsAvailable: !!this.generation && !this.switching,
        access: session ? this.accessPreference?.mode ?? this.staged?.data?.sandbox : context.view?.setup?.agent?.sandbox,
        accessDisabled: !!this.submitting || !!editor?.record.pending || !!this.switching,
        text: starting?.text ?? editor?.text ?? "",
        workspace: startingWorkspace(this, context)?.id,
        disabled:
          !!(this.switching || (!this.generation ? !starting || this.submitting : !editor || editor.transferring || editor.resolving)),
        sendDisabled: starting ? !!starting.failure || this.submitting || this.switching || !starting.text.trim() : !!sendDisabled,
        alternativeDisabled:
          !!sendDisabled || !!pending || !this.composerActions?.alternative,
        alternative: this.composerActions?.alternative,
        sendLabel: pending
          ? pending.phase === "prepared"
            ? "Send saved request"
            : pending.kind !== "command"
              ? "Retry previous"
              : "Check command result"
          : (this.composerActions?.primary.label ?? "Send"),
        exporting: !!this.exportAbort,
        bindingError: this.bindingError,
        storageNotice: context.connection?.storageNotice,
        discardConfirmed: !!this.discardDraftsConfirmed,
        discarding: this.discarding,
        failure: starting?.failure ? {message:starting.failure.message, resolve:useSaved=>starting.resolve(useSaved)} : editor?.failure
          ? {
              message: editor.failure.message,
              resolve: (useSaved) => editor.resolve(useSaved),
            }
          : undefined,
        dirty: starting?.dirty ?? editor?.dirty,
        pending,
        cleanups: [...(this.referenceCleanup ?? [])]
          .filter(
            ([, cleanup]) =>
              cleanup.message &&
              (!cleanup.namespace ||
                (!context.connection?.closing &&
                  JSON.stringify(context.connection?.drafts?.namespace) ===
                    cleanup.namespace)),
          )
          .map(([id, cleanup]) => ({
            id,
            message: cleanup.message,
            retry: async () => {
              const owner = await cleanup.resolveEditor();
              return owner?.record?.releases?.some(capture => this.cleanupKey(owner, capture) === id)
                ? this.drainReferenceReleases(owner, cleanup.generation, true)
                : cleanup.retry();
            },
          })),
        images: this.images,
        references: (editor?.references ?? []).map((reference) => {
          const interval = context.referenceInterval(reference.metadata);
          return {
            id: captureKey(captureOf(reference)),
            label: `${context.referenceOrigin(reference.metadata)} · through record ${interval.through_seq}${interval.omissions.length ? " · shortened" : ""}`,
          };
        }),
        fileDisabled: !!fileDisabled,
        referenceDisabled: !!fileDisabled || !canCaptureReference(editor.record, editor.references),
        referenceRemovalDisabled: !!this.submitting || !!this.switching || !!editor?.resolving || !!(editor && this.referenceState(editor).preparing),
        imageDisabled:
          !this.staged?.data ||
          !editor ||
          editor.resolving ||
          this.uploading ||
          this.submitting ||
          this.switching,
        mediaEnabled: !!context.view?.has_media,
      };
      this.renderCompletions(context.view?.surfaces[this.index]);
      this.invalidate();
    }
    async discardSavedDrafts() {
      if (this.discarding) return;
      if (!this.discardDraftsConfirmed) {
        this.discardDraftsConfirmed = true;
        this.renderComposer();
        return;
      }
      this.discarding = true;
      this.renderComposer();
      try {
        await context.DraftStore.discardAllSaved();
        context.location.reload();
      } catch (error) {
        context.notify(`Draft reset failed: ${error.message}`);
      } finally {
        this.discarding = false;
        this.renderComposer();
      }
    }
    cancelPrepared() {
      const editor = this.editor;
      return editor?.update((record) => editor.store.cancelPrepared(record));
    }
    previewReference(id) {
      const reference = this.editor?.references.find(
        (item) => captureKey(captureOf(item)) === id,
      );
      if (reference) this.showReferencePicker(reference);
    }
    async removeReference(id) {
      const editor = this.editor,
        reference = editor?.references.find(
          (item) => captureKey(captureOf(item)) === id,
        );
      if (!reference) return;
      const state = this.referenceState(editor);
      if (this.submitting || this.switching || state.preparing)
        throw new Error("Wait for this conversation’s current operation");
      state.holdCleanup++;
      const cleanup = this.referenceCleanupOwner(reference, editor, this.generation);
      try {
      this.edit(
        editor.text,
        editor.images,
        editor.references.filter((item) => captureKey(captureOf(item)) !== id),
      );
      this.input.focus();
      await editor.flush();
      await editor.update(record => editor.store.retireUnowned(record, [{target: reference.metadata.target, snapshot: reference.snapshot}]));
      } catch (error) {
        cleanup.message = `Reference cleanup requires retry: ${error.message}`;
        this.renderComposer();
        throw error;
      } finally { state.holdCleanup--; }
      await this.drainReferenceReleases(editor, this.generation).catch(() => {});
    }
    moveImage(index, to) {
      const editor = this.editor;
      if (!editor || index < 0 || index >= editor.images.length) return;
      const images = [...editor.images],
        [media] = images.splice(index, 1);
      if (to !== null) images.splice(to, 0, media);
      this.edit(editor.text, images);
    }
    previewImage(index) {
      return this.action("inspect_image", { media: this.editor.images[index] });
    }
    showFilePicker() {
      return showFilePicker(this, context);
    }
    showHistoryPicker() {
      return showHistoryPicker(this, context);
    }
    showReferencePicker(existing) {
      return showReferencePicker(this, context, existing);
    }
    async exportSession(args) {
      return exportSession(this, context, args);
    }
    async submit(choice = "primary") {
      return submit(this, context, choice);
    }
    async reconcile(mode) {
      return reconcile(this, context, mode);
    }
    async executePending(editor, generation, current, mode) {
      return executePending(this, context, editor, generation, current, mode);
    }
    renderQueue(data) {
      if (data?.protected) {
        this.queueBinding = undefined;
        this.queueDialog?.close();
      }
    }
    async queueAction(slot, action) {
      const binding = this.queueBinding;
      const item = binding?.items.find((item) => item.queue_slot.id === slot);
      if (!item || binding.generation !== this.generation)
        throw new Error("Wait for the current queue view");
      const queue = {
        slot_id: slot,
        expected_message_id: item.message_id,
        action,
      };
      if (action === "convert_to_steer") {
        if (!item.convert_turn)
          throw new Error("The displayed Turn is unavailable");
        queue.expected_turn_id = item.convert_turn;
      }
      if (action !== "read") return this.submitQueue(queue, binding.generation);
      const trigger = document.activeElement,
        generation = binding.generation,
        current = context.connection;
      const detail = JSON.parse(
        await context.call(
          "prepare_submission",
          JSON.stringify({ pane: this.index, generation, queue }),
        ),
      );
      if (
        generation !== this.generation ||
        current !== context.connection ||
        current.closing
      )
        return;
      this.queueDialog?.close();
      const editor = createQueueEditor({
        content: detail.content,
        editable: item.editable,
        submit: (content) =>
          this.submitQueue(
            { ...queue, action: "replace", content },
            generation,
          ),
        changed: () => this.invalidate(),
        closed: () => {
          if (this.queueDialog === editor) this.queueDialog = undefined;
          this.invalidate();
        },
      });
      editor.restoreFocus = () => {
        if (
          this.generation === generation &&
          context.connection === current &&
          !current.closing
        )
          (trigger?.isConnected ? trigger : this.input)?.focus();
      };
      editor.id = String(
        (this.queueDialogSequence = (this.queueDialogSequence ?? 0) + 1),
      );
      this.queueDialog = editor;
      this.invalidate();
    }

    async submitQueue(queue, generation) {
      return submitQueue(this, context, queue, generation);
    }
    async upload(files) {
      return upload(this, context, files);
    }
    async restoreDrafts() {
      return restoreDrafts(this, context);
    }
    async recreateDraft(record, current) {
      return recreateDraft(this, context, record, current);
    }
    reset() {
      this.startBindingOwner = undefined;
      this.startBinding = undefined;
      this.starting = undefined;
      this.staged = undefined;
      this.discardDraftsConfirmed = false;
      this.switching = false;
      this.exportAbort?.abort();
      this.referenceDialog?.close();
      this.historyDialog?.close();
      this.fileDialog?.close();
      this.recoveryView = undefined;
      this.editor = undefined;
      if (this.input) this.input.value = "";
      this.render(null);
    }
    completionToken() {
      return completionToken(this, context);
    }
    updateCompletions(refresh = false) {
      return updateCompletions(this, context, refresh);
    }
    closeCompletions() {
      return closeCompletions(this, context);
    }
    completionEntries(data = context.view?.surfaces[this.index]) {
      return completionEntries(this, context, data);
    }
    pickCompletion(item) {
      return pickCompletion(this, context, item);
    }
    completionKey(event) {
      return completionKey(this, context, event);
    }
    renderCompletions(data) {
      return renderCompletions(this, context, data);
    }
    render(data) {
      const connection = context.connection;
      if (!data && connection?.drafts && !connection.closing &&
          (this.startBindingOwner?.connection !== connection || (!this.starting && !this.startBinding))) {
        const binding = {connection};
        this.startBindingOwner = binding;
        this.starting = undefined;
        this.startBinding = bindStarting(this, context, binding).catch(error => {
          if (this.startBindingOwner !== binding) return;
          this.startBindingOwner = undefined;
          context.notify(error.message);
        }).finally(() => {
          if (this.startBindingOwner === binding) this.startBinding = undefined;
        });
      }
      this.historyContext = data
        ? { session: data.session, workspace: data.workspace_key }
        : undefined;
      const changed = this.generation !== data?.generation;
      if (this.selection !== data?.selection) {
        this.selection = data?.selection;
        this.switching = false;
      }
      if (changed) {
        this.commandsExpanded = false;
        this.queueDialog?.close();
        this.queueBinding = undefined;
        this.referenceDialog?.close();
        this.historyDialog?.close();
        this.fileDialog?.close();
        this.switching = false;
        this.exportAbort?.abort();
        this.generation = data?.generation;
        this.completionState = undefined;
        this.completionRequestKey = undefined;
        this.completionFailure = undefined;
        this.completions = undefined;
        clearTimeout(this.completionTimer);
        this.binding = this.bindDraft(data).catch((error) =>
          context.notify(error.message),
        );
        this.lastTranscript = undefined;
        this.readingSummary = false;
        this.visibleBlocks?.update([], this.generation);
        this.visibleKey = undefined;
      }
      this.historical = !!data?.historical;
      this.uiCards = !!data?.ui_cards;
      this.uiRevision = data?.ui_revision;
      if (
        !data ||
        this.stopBinding?.generation !== data.generation ||
        this.stopBinding?.turn_id !== data.active
      )
        this.stopBinding = undefined;
      this.renderQueue(data);
      this.renderComposer();
      if (!data) {
        return;
      }
      this.renderTranscript(data.transcript);
      this.renderInline(data.inline);
      this.scheduleVisible();
    }

    renderTranscript(transcript) {
      this.lastTranscript = transcript;
      this.visibleBlocks.update(
        transcript.blocks
          .map((block) => [block.key, this.blocks.get(block.key)])
          .filter(([, entry]) => entry),
        this.generation,
      );
    }
  };
}
