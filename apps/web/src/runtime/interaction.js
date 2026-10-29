import { decisionAction } from "../../approval-review.js";
export function interactionKey(data) {
  return JSON.stringify([data.pane, data.generation, data.kind,
    data.kind === 'approval' ? data.request.subject.session_id : data.request.session_id,
    data.request.id]);
}
/** @param {{command: (value: Record<string, unknown>) => Promise<unknown>, refreshCommand: Record<string, unknown>, ticket?: string}} options */
export function createInteraction({ command, refreshCommand, ticket }) {
  let revision = 0;
  let latestTicket = ticket, refreshTicket;
  const listeners = new Set(),
    changed = () => {
      revision++;
      for (const listener of listeners) listener();
    };
  const state = {
    locked: false,
    pending: false,
    refreshing: false,
    refreshed: false,
    error: "",
    subscribe: (listener) => {
      listeners.add(listener);
      return () => listeners.delete(listener);
    },
    getSnapshot: () => revision,
    get disabled() {
      return state.locked || state.refreshing || state.refreshed;
    },
    update(next) {
      command = next.command;
      refreshCommand = next.refreshCommand;
      latestTicket = next.ticket;
      settleRefresh();
    },
    decide(value) {
      if (state.refreshing || state.refreshed || state.pending) return;
      return decide(value);
    },
    report(message) {
      state.error = message;
      changed();
    },
    async refresh() {
      if (state.pending || state.refreshing) return;
      refreshTicket = latestTicket;
      state.refreshed = false;
      state.refreshing = true;
      changed();
      try {
        await command(refreshCommand);
        state.refreshed = true;
      } catch (error) {
        state.error = `Pending-state read failed: ${error.message}`;
      } finally {
        state.refreshing = false;
        settleRefresh();
        changed();
      }
    },
  };
  const newDecision = () => decisionAction(
    value => command(value),
    (value) => {
      state.locked = value;
      changed();
    },
    state.report,
    (value) => {
      state.pending = value;
      changed();
    },
  );
  let decide = newDecision();
  function settleRefresh() {
    if (!state.refreshed || state.refreshing || latestTicket === refreshTicket) return;
    state.locked = false;
    state.refreshed = false;
    state.error = '';
    decide = newDecision();
    changed();
  }
  return state;
}
