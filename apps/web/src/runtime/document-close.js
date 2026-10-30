import {closeStage} from '../../document-connection.js';

export function createDocumentClose(context) {
  const nativeCloseAttempts = [];
  async function acknowledgeNativeClose(ready) {
    while (nativeCloseAttempts.length) {
      const attempt = nativeCloseAttempts.shift();
      await context.nativeCall({ kind: "close", attempt, ready });
    }
  }
  async function cancelNativeCloseAfterDraftFailure(current, draftError) {
    try {
      await acknowledgeNativeClose(false);
      if (context.connection !== current || current.closing) return false;
      return true;
    } catch (error) {
      if (context.connection === current && !current.closing) {
        context.failWorker(
          `Draft is not saved: ${draftError.message}. Could not cancel native close: ${error.message}. Recover the draft before closing.`,
          current,
        );
      }
      return false;
    }
  }

  async function flushCapturedPanes(captured) {
    const saved = await Promise.allSettled(
      captured.map((pane) => pane.flush(true)),
    );
    const failed = saved.find((result) => result.status === "rejected");
    if (failed) throw failed.reason;
  }

  async function disconnectDocument() {
    const current = context.connection;
    if (!current || current.phase === "Closed") {
      await acknowledgeNativeClose(true);
      return;
    }
    if (current.phase === "Failed") {
      // Native listener EOF and Worker failure can arrive in either order. Editors
      // retain their local drafts after view reset; persist them before native exit.
      try {
        await closeStage(flushCapturedPanes([...context.panes.values()]), 'Draft persistence');
      } catch (error) {
        await acknowledgeNativeClose(false);
        context.notify(
          `Draft is not saved: ${error?.message ?? error}. Keep this window open and recover the draft.`,
        );
        return;
      }
    }
    if (current.closing) {
      try {
        await current.settled();
        if (
          context.connection === current &&
          ["Closed", "Failed"].includes(current.phase)
        )
          await acknowledgeNativeClose(true);
      } catch (error) {
        await acknowledgeNativeClose(false);
        throw error;
      }
      return;
    }
    context.setClosing(true);
    // Capture the actual editors before the first await, not replacement panes.
    const capturedPanes = [...context.panes.values()];
    try {
      let draftFailure,
        draftFailed = false;
      const resources = await current.disconnect(async () => {
        try {
          await flushCapturedPanes(capturedPanes);
        } catch (error) {
          draftFailed = true;
          draftFailure = error;
          throw error;
        }
      });
      if (context.connection === current && draftFailed) {
        await acknowledgeNativeClose(false);
        context.notify(
          `Draft is not saved: ${draftFailure?.message ?? draftFailure}. Keep this window open and recover the draft.`,
        );
        return;
      }
      if (context.connection !== current) return;
      if (current.phase === "Failed") {
        await current.settled();
        if (context.connection === current) await acknowledgeNativeClose(true);
        return;
      }
      if (current.phase !== "Closed") return;
      context.disconnected(resources);
      await acknowledgeNativeClose(true);
    } catch (error) {
      if (context.connection !== current) return;
      if (error.draftSaveFailed && !current.closing) {
        if (context.nativeDocument) {
          if (!(await cancelNativeCloseAfterDraftFailure(current, error)))
            return;
        }
        context.notify(
          `Draft is not saved: ${error.message}. Recover the draft before closing.`,
        );
      } else {
        context.failWorker(String(error.message ?? error), current);
        await current.settled();
        if (context.connection === current) await acknowledgeNativeClose(true);
      }
    } finally {
      if (context.connection === current) context.setClosing(false);
    }
  }

  return {
    disconnectDocument,
    cancelNativeCloseAfterDraftFailure,
    get attempt() {
      return nativeCloseAttempts[0];
    },
    set attempt(value) {
      if (value !== undefined && !nativeCloseAttempts.includes(value)) nativeCloseAttempts.push(value);
    },
  };
}
