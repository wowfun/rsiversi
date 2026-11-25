import {bindStarting, startingWorkspace, startConversation, prepareComposer, selectWorkspace} from './starting-conversation.js';
import {assertDraftStorageActive, draftStorageRetired} from '../../draft-databases.js';
import {commandSelection, claimedCommand, commandToken} from '../../command-input.js';
import {formatQuoteText} from '../../vendor/dsh/quote/format-quote.ts';
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
function showsQueuedMessage(data, messageId) {
  return !data?.protected && data?.queue?.some(item => item.message_id === messageId);
}
// The Session lifetime and commands use live owner ports across asynchronous work.
export function createPaneClass(context) {
  return class Pane {
    constructor(index) {
      this.index = index;
      this.blocks = new Map();
      this.visibleSequence = 0n;
      this.visibleKey = "";
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
    insertQuote = text => {
      const input = this.input;
      if (!this.editor || this.composing || this.switching || input.disabled || input.readOnly) return false;
      const retained = this.retainedSelection;
      const end = retained?.editor === this.editor ? retained.end : input.value.length;
      const length = input.value.length;
      const quote = formatQuoteText(text) + ' ';
      input.focus({preventScroll: true});
      input.setSelectionRange(end, end);
      // Native insertion joins typing history and uses the ordinary draft input handler.
      return input.ownerDocument.execCommand('insertText', false, quote) && input.value.length === length + quote.length;
    };
    captureCommandSelection() {
      this.commandSelection = {editor:this.editor,generation:this.generation,revision:this.editor?.revision,
        start:this.input.selectionStart ?? 0,end:this.input.selectionEnd ?? 0};
    }
    async selectCommand(command, selection = this.commandSelection) {
      if (!this.editor) throw new Error('The draft is not ready; wait for it to load');
      if (!selection) { this.captureCommandSelection(); selection=this.commandSelection; }
      this.commandSelection = undefined;
      if (this.composing || selection.editor !== this.editor || selection.generation !== this.generation || selection.revision !== this.editor.revision)
        throw new Error('The input changed; select the command again');
      const selected = commandSelection(this.input.value,selection.start,selection.end,command,selection.trigger);
      if (selected.kind === 'execute') {
        await executeDocumentCommand(this,context,command.name,'',selected.consume,selection.revision);
      } else {
        this.edit(selected.text);
        this.selectedCommand = command;
        this.input.value = selected.text;
        this.input.setSelectionRange?.(`/${command.name} `.length,`/${command.name} `.length);
      }
      this.closeCompletions();
      this.input.focus();
    }
    clearCommand() {
      if (this.composerView?.disabled || this.composing) return;
      const command = this.composerView.command;
      if (!command) return;
      this.edit(this.editor.text.slice(command.end));
      this.focusInput();
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
      if (action === "document_resource" || action === "read_pending_interaction") {
        const connection = context.connection;
        const method = action === "document_resource" ? "read_session_resource" : "read_pending_interaction";
        const response = await context.call(method, JSON.stringify({...fields,pane:this.index,generation}));
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
      if (this.visibleFrame || (!this.uiCards && this.visibleKey === "")) return;
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
      const key = entries.length ? JSON.stringify([
        this.generation,
        this.historical,
        this.uiRevision,
        entries.map(([key, entry]) => [key, entry.sourceKey]),
      ]) : "";
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
      if (!cards) return;
      for (const [key, card] of Object.entries(cards)) {
        const entry = this.blocks.get(key);
        if (!entry || !card?.model || !card.ticket) continue;
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
      assertDraftStorageActive();
      if (!this.generation && this.starting) { this.starting.edit(text); return; }
      if (!this.editor) throw new Error("The saved draft is still loading");
      this.editor.edit(text, images, references);
      this.prefetchCommands(text);
    }
    prefetchCommands(text) {
      const generation=this.generation, current=context.connection;
      if (!generation || !commandToken(text) || this.staged?.data?.commands ||
        (this.commandCatalog?.generation === generation && this.commandCatalog.connection === current)) return;
      const read={generation,connection:current};
      this.commandCatalog=read;
      void this.action('commands').catch(error=>{
        if (this.generation === generation && context.connection === current) context.notify(error.message);
      }).finally(()=>{
        if (this.commandCatalog === read) this.commandCatalog=undefined;
      });
    }
    async bindDraft(data) {
      const generation = this.generation,
        current = context.connection;
      const previous = this.editor;
      this.editor = undefined;
      if (previous?.text) {
        const work = previous.work, key = JSON.stringify(previous.record.key);
        void work.then(() => {
          if (previous !== this.editor && previous.work === work && previous.savedTextOnly &&
              this.editors.get(key) === previous) this.editors.delete(key);
        });
      }
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
          const change = this.pendingPresetChange;
          if (change && change.connection === current && data.session === change.session && data.agent_preset === change.preset && data.generation !== change.generation) {
            if (record.header !== data.header) record = await store.rebindPreset(change.record, data.header, change.preset);
            if (this.pendingPresetChange === change) this.pendingPresetChange = undefined;
          }
        } catch (error) {
          failure = error;
          record =
            editor?.record ??
            store.blank(this.index, data.session, data.header, data.creation);
        }
        editor = new context.DraftEditor(store, record);
        editor.failure = failure;
      }
      if (
        this.generation !== generation ||
        context.connection !== current ||
        current.closing
      )
        return;
      this.editors.set(key, editor);
      this.editor = editor;
      editor.reuseGeneration = data.creation && !editor.record.everDispatched ? generation : undefined;
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
      if (!data.commands) this.prefetchCommands(editor.text);
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
    selectWorkspace(id) { return selectWorkspace(this, context, id); }
    startConversation() { return startConversation(this, context); }
    async selectPreset(id) {
      if (this.submitting || this.switching || this.uploading) throw new Error('Wait for the current operation before selecting a preset');
      if (!this.generation) {
        this.stagedPreset = id;
        this.invalidate();
        return;
      }
      const editor = this.editor, generation = this.generation, data = this.staged.data;
      if (id === data.agent_preset) return;
      this.switching = true;
      this.renderComposer();
      try {
        await this.flush();
        if (!editor || this.editor !== editor || this.generation !== generation) throw new Error('The draft changed while selecting a preset');
        if (editor.record.pending) throw new Error('Resolve pending input before changing the preset');
        if (data.projections?.cursor?.kind !== 'durable' && data.draft !== false) {
          if (editor.record.references.length || editor.record.releases.length)
            throw new Error('Release references before changing the draft preset');
          this.pendingPresetChange = {connection:context.connection, generation, session:data.session, preset:id, record:editor.record};
        }
        await this.action('select_preset', {header:editor.record.header, preset:id});
      } catch (error) {
        if (this.generation === generation) {
          this.pendingPresetChange = undefined;
          this.switching = false;
          this.renderComposer();
        }
        throw error;
      }
    }
    prepareComposer() { return prepareComposer(this, context); }
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
      const data = this.staged?.data;
      const storageRetired = draftStorageRetired();
      const starting = !this.generation ? this.starting : undefined;
      const session = this.staged?.data?.session;
      const permission = data?.projections?.entries?.find(entry=>entry.producer==='rsi.permission.view')?.content?.value;
      const selection = session ? permission : context.view?.setup?.agent?.permission_default;
      const sandbox = session ? permission?.sandbox : selection?.sandbox ?? context.view?.setup?.agent?.sandbox;
      const standard = (selection?.approval ?? 'ask') === (sandbox === 'danger-full-access' ? 'never' : 'ask');
      const editor = this.editor,
        pending = editor?.record.pending;
      const sendDisabled =
        storageRetired ||
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
      const disabled = !!(storageRetired || this.switching ||
        (!this.generation ? !starting || this.submitting : !editor || editor.transferring || editor.resolving));
      this.images = editor?.images ?? [];
      this.composerView = {
        completionsAvailable: !!this.generation && !this.switching,
        access: sandbox && (standard && (!selection?.preset || selection.preset === sandbox) ? sandbox : 'custom'),
        accessDisabled: !(context.view?.navigation?.can_archive ?? data?.can_stop_work ?? context.view?.setup?.allowed)
          || (!session && !context.view?.setup?.agent?.default_model)
          || !!this.submitting || !!editor?.record.pending || !!this.switching,
        text: starting?.text ?? editor?.text ?? "",
        command: claimedCommand(editor?.text ?? '', [...(this.staged?.data?.commands?.commands ?? []),...(this.selectedCommand ? [this.selectedCommand] : [])]),
        workspace: data?.workspace?.registration.status === 'registered' ? data.workspace.registration.id : data ? undefined : startingWorkspace(this, context)?.id,
        bindingInput: !!(this.submitting && !this.switching && !storageRetired && (!this.generation || !editor)),
        disabled,
        sendDisabled: storageRetired || (starting ? !!starting.failure || !!this.submitting || this.switching || !starting.text.trim() : !!sendDisabled),
        sendBusy: !!this.submitting,
        alternativeDisabled:
          !!sendDisabled || !!pending || !this.composerActions?.alternative,
        alternative: this.composerActions?.alternative,
        sendLabel: this.submitting ?? (pending
          ? pending.phase === "prepared"
            ? "Send saved request"
            : pending.kind !== "command"
              ? "Retry previous"
              : "Check command result"
          : (this.composerActions?.primary.label ?? "Send")),
        exporting: !!this.exportAbort,
        bindingError: this.bindingError,
        storageNotice: storageRetired ? 'Draft storage is retired. Finish the reset or reload before editing.' : context.connection?.storageNotice ?? this.startingStorageError,
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
        deliveryError: pending && editor.deliveryDetail?.id === pending.id ? editor.deliveryDetail?.error : undefined,
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
        generation: this.generation,
        importingImages: this.uploading?.editor === editor ? this.uploading : undefined,
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
        imageDisabled: disabled || !!this.uploading || !!this.submitting,
        mediaLimits: context.view?.has_media ? context.view.media_limits : undefined,
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
        await context.DraftStore.discardAllDrafts();
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
    acquireDraftImage(media, generation) {
      if (this.generation !== generation || this.bindingError || !this.editor)
        throw new Error("This draft is unavailable");
      return context.previews(context.connection).acquire(media, {kind:"draft", pane:this.index, generation, media});
    }
    acquireTranscriptImage(media, generation, key, index) {
      if (this.generation !== generation) throw new Error("This conversation changed");
      return context.previews(context.connection).acquire(media, {kind:"transcript", pane:this.index, generation, key, index});
    }
    acquireQueueImage(media, generation, slot, message, index) {
      if (this.generation !== generation || !showsQueuedMessage(this.staged?.data, message))
        throw new Error("This queued image is no longer available");
      return context.previews(context.connection).acquire(media, {kind:"queue", pane:this.index, generation, slot, message, index});
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
      if (data?.protected) this.queueBinding = undefined;
      if (this.queueContents?.size) {
        const current = new Set(data?.protected ? [] : data?.queue?.map(item => item.message_id));
        for (const id of this.queueContents.keys()) {
          if (!current.has(id)) this.queueContents.delete(id);
        }
      }
      if (this.queueDialog && !showsQueuedMessage(data, this.queueDialog.messageId))
        this.queueDialog.close();
    }
    async readQueueContent(slot, messageId) {
      const generation = this.generation;
      const binding = this.queueBinding;
      if (binding?.generation !== generation ||
          !binding?.items.some(item => item.queue_slot.id === slot && item.message_id === messageId))
        throw new Error("Wait for the current queue view");
      const contents = this.queueContents ??= new Map();
      if (!contents.has(messageId)) {
        const items = binding.items.filter(row => !contents.has(row.message_id))
          .map(row => ({slot_id:row.queue_slot.id, expected_message_id:row.message_id}));
        const queue = {action:"read",items};
        const read = context.call("prepare_submission", JSON.stringify({pane:this.index, generation, queue}))
          .then(reply => {
            const rows = JSON.parse(reply);
            items.forEach(({expected_message_id:id}, index) => {
              if (rows[index] && contents.get(id) === read) contents.set(id, rows[index]);
            });
          }).finally(() => {
            for (const {expected_message_id:id} of items) {
              if (contents.get(id) === read) contents.delete(id);
            }
          });
        for (const item of items) contents.set(item.expected_message_id, read);
      }
      const cached = contents.get(messageId);
      if (Array.isArray(cached)) return cached;
      await cached;
      return contents.get(messageId) ?? null;
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
      const sequence = (this.queueDialogSequence ?? 0) + 1;
      this.queueDialogSequence = sequence;
      const content = await this.readQueueContent(slot, item.message_id);
      if (
        generation !== this.generation ||
        current !== context.connection ||
        current.closing ||
        sequence !== this.queueDialogSequence ||
        !content || !showsQueuedMessage(this.staged?.data, item.message_id)
      )
        return;
      this.queueDialog?.close();
      const editor = createQueueEditor({
        content,
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
      editor.messageId = item.message_id;
      if (item.editable && content.length === 1 && content[0].type === "text")
        editor.inlineSlot = slot;
      editor.restoreFocus = () => {
        if (
          this.generation === generation &&
          context.connection === current &&
          !current.closing && !this.queueDialog
        )
          (trigger?.isConnected ? trigger : this.input)?.focus();
      };
      editor.id = String(sequence);
      this.queueDialog = editor;
      this.invalidate();
    }

    async submitQueue(queue, generation) {
      return submitQueue(this, context, queue, generation);
    }
    async upload(files) {
      if (await this.prepareComposer()) await upload(this, context, files);
    }
    async restoreDrafts() {
      return restoreDrafts(this, context);
    }
    async recreateDraft(record, current) {
      return recreateDraft(this, context, record, current);
    }
    reset() {
      this.pendingPresetChange = undefined;
      this.stagedPreset = undefined;
      this.startBindingOwner = undefined;
      this.startBinding = undefined;
      this.starting = undefined;
      this.startingStorageError = undefined;
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
          this.startingStorageError = error.message;
          this.renderComposer();
          context.notify(error.message);
        }).finally(() => {
          if (this.startBindingOwner === binding) this.startBinding = undefined;
        });
      }
      this.historyContext = data
        ? { session: data.session, workspace: data.workspace.key }
        : undefined;
      const changed = this.generation !== data?.generation;
      if (this.selection !== data?.selection) {
        this.selection = data?.selection;
        this.switching = false;
      }
      if (changed) {
        if (this.pendingPresetChange?.session !== data?.session) this.pendingPresetChange = undefined;
        this.stagedPreset = undefined;
        this.commandSelection = undefined;
        this.selectedCommand = undefined;
        this.commandsExpanded = false;
        this.queueDialog?.close();
        this.queueBinding = undefined;
        this.queueContents = undefined;
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
        this.visibleKey = "";
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
      const entries = [];
      if (this.uiCards) {
        for (const block of transcript.blocks) {
          const entry = this.blocks.get(block.key);
          if (entry) entries.push([block.key, entry]);
        }
      }
      this.visibleBlocks.update(entries, this.generation);
    }
  };
}
