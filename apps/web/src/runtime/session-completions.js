export function completionToken(pane, context) {
  const text = pane.input.value,
    cursor = pane.input.selectionStart;
  if (
    pane.composing ||
    !pane.generation ||
    pane.switching ||
    pane.input.selectionEnd !== cursor
  )
    return;
  return context.completionToken(text, cursor);
}

export function updateCompletions(pane, context, refresh = false) {
  const token = pane.completionToken();
  if (!token) {
    pane.closeCompletions();
    pane.completionRequestKey = undefined;
    return;
  }
  refresh ||= !pane.completionState || pane.completionState.refreshPending;
  const key = JSON.stringify([pane.generation, token.text, token.cursor]);
  if (!refresh && key === pane.completionRequestKey) return;
  pane.completionRequestKey = key;
  pane.completionFailure = undefined;
  pane.completionState = {
    ...token,
    sequence: String(
      (pane.completionSequence = (pane.completionSequence ?? 0) + 1),
    ),
    generation: pane.generation,
    selected: 0,
    refreshPending: refresh,
  };
  const state = pane.completionState;
  clearTimeout(pane.completionTimer);
  pane.renderCompletions(context.view?.surfaces[pane.index]);
  pane.completionTimer = setTimeout(() => {
    if (pane.completionState !== state) return;
    state.refreshPending = false;
    void pane
      .action("completions", {
        query: state.query,
        sequence: state.sequence,
        refresh,
      })
      .catch((error) => {
        if (pane.completionState === state) {
          pane.completionFailure = error.message;
          pane.renderCompletions(context.view?.surfaces[pane.index]);
        }
      });
  }, 80);
}

export function closeCompletions(pane, context) {
  clearTimeout(pane.completionTimer);
  pane.completionState = undefined;
  if (pane.completions) {
    pane.completions = undefined;
    pane.invalidate();
  }
}

export function completionEntries(
  pane,
  context,
  data = context.view?.surfaces[pane.index],
) {
  const state = pane.completionState,
    result = data?.completions;
  if (
    !state ||
    state.generation !== pane.generation ||
    result?.sequence !== state.sequence ||
    result.query !== state.query
  )
    return;
  return result.entries;
}

export function pickCompletion(pane, context, item) {
  const state = pane.completionState,
    token = pane.completionToken();
  if (
    !state ||
    !token ||
    state.text !== token.text ||
    state.cursor !== token.cursor ||
    state.generation !== pane.generation
  )
    return;
  pane.input.setRangeText(item.replacement, token.start, token.end, "end");
  pane.edit(pane.input.value);
  pane.completionRequestKey = JSON.stringify([
    pane.generation,
    pane.input.value,
    pane.input.selectionStart,
  ]);
  pane.closeCompletions();
  pane.input.focus();
}

export function completionKey(pane, context, event) {
  if (event.key === "Escape" && !pane.resourcePreview.hidden) {
    event.preventDefault();
    context.perform(() => pane.action("resource_close"));
    pane.input.focus();
    return true;
  }
  if (!pane.completionState || pane.completionPanel.hidden) return false;
  if (event.key === "Escape") {
    event.preventDefault();
    pane.closeCompletions();
    return true;
  }
  const entries = pane.completionEntries();
  if (!entries?.length) return false;
  if (event.key === "ArrowUp" || event.key === "ArrowDown") {
    event.preventDefault();
    pane.completionState.selected = Math.max(
      0,
      Math.min(
        entries.length - 1,
        pane.completionState.selected + (event.key === "ArrowDown" ? 1 : -1),
      ),
    );
    pane.renderCompletions(context.view?.surfaces[pane.index]);
    return true;
  }
  if (
    (event.key === "Tab" || event.key === "Enter") &&
    !event.shiftKey &&
    !event.ctrlKey &&
    !event.metaKey
  ) {
    event.preventDefault();
    pane.pickCompletion(entries[pane.completionState.selected]);
    return true;
  }
  if (event.key === "F2" && entries[pane.completionState.selected].resource) {
    event.preventDefault();
    context.perform(() =>
      pane.action("resource_read", {
        request: entries[pane.completionState.selected].resource,
      }),
    );
    return true;
  }
  return false;
}

export function renderCompletions(pane, context, data) {
  const state = pane.completionState;
  if (!state || state.generation !== pane.generation) {
    pane.closeCompletions();
    return;
  }
  const entries = pane.completionEntries(data);
  const next = {
    entries,
    selected: state.selected,
    failure: pane.completionFailure,
    notice:
      data?.completions?.sequence === state.sequence
        ? data.completions.notice
        : undefined,
  };
  pane.completions = next;
  pane.invalidate();
}
