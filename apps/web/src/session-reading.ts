import {useLayoutEffect, useRef} from "react";
import {input, run, useView, type AttentionEntry, type Navigation} from "./bridge.ts";
import type {NativeSurface} from "./presentation-types.ts";

export function markReading(host_epoch: string, session: string, row: AttentionEntry | undefined, action: "read" | "unread" | "acknowledge") {
  const position = row?.position ?? {conversation: {kind: "native", id: session}, epoch: "0", sequence: "0"};
  return input.command({action: "navigate", command: {kind: "mark", request: {host_epoch, position, action}}});
}

export function isUnread(row?: AttentionEntry) {
  return !!row && (row.reading.manual || row.reading.unseen);
}

function archived(nav: Navigation, session: string) {
  return nav.entries.some(entry => entry.session === session && entry.metadata.archived)
    || nav.pins.some(pin => pin.status === "available" && pin.entry.session === session && pin.entry.metadata.archived)
    || Object.values(nav.groups).some(group => group.entries.some(entry => entry.session === session && entry.metadata.archived));
}

// Mounted for the selected conversation. Entry and explicit user intent have distinct lifetimes.
export function SessionReading({surface, visible}: {surface: NativeSurface; visible: boolean}) {
  const session = surface.session;
  const nav = useView(view => view?.navigation);
  const page = nav?.attention_notice ? undefined : nav?.attention;
  const row = page?.entries.find(entry => entry.position.conversation.kind === "native" && entry.position.conversation.id === session);
  const latest = useRef({nav, page, row, surface, visible});
  const schedule = useRef<(() => void) | undefined>(undefined);
  const visit = useRef<{epoch?: string; entered: boolean; attempted?: string}>({entered: true});

  useLayoutEffect(() => {
    latest.current = {nav, page, row, surface, visible};
    if (page) {
      if (page.host_epoch !== visit.current.epoch) visit.current = {epoch: page.host_epoch, entered: true};
      if (!row?.reading.manual) visit.current.entered = false;
    }
    schedule.current?.();
  }, [nav, page, row, surface, visible]);

  useLayoutEffect(() => {
    let frame: number | undefined;
    let pending = false;
    const focused = () => document.visibilityState === "visible" && document.hasFocus();
    const refresh = () => {
      if (frame === undefined && !pending && latest.current.visible && focused()) {
        frame = requestAnimationFrame(acknowledge);
      }
    };
    const acknowledge = () => {
      frame = undefined;
      const {page, row, surface, visible} = latest.current;
      const entry = visit.current;
      if (!page || !row || !visible || !focused() || (!entry.entered && !row.reading.unseen)) return;
      const through = row.position.sequence;
      if (entry.attempted === through) return;
      if (through !== "0" && !Object.values(surface.transcript.turns?.entries ?? {}).some(turn => {
        const seq = turn.terminal_sequence;
        return seq && (seq.length > through.length || seq.length === through.length && seq >= through);
      })) return;
      entry.attempted = through;
      const action = entry.entered ? "read" : "acknowledge";
      entry.entered = false;
      pending = true;
      void run(() => markReading(page.host_epoch, session, row, action)).finally(() => {
        pending = false;
        schedule.current?.();
      });
    };
    const focus = () => { visit.current.attempted = undefined; refresh(); };
    const toggle = (event: KeyboardEvent) => {
      if (event.defaultPrevented || event.repeat || event.isComposing || event.code !== "KeyU" || !event.shiftKey
        || event.altKey || event.ctrlKey === event.metaKey) return;
      const {page, row, nav, visible} = latest.current;
      if (!page || !nav || !visible || archived(nav, session)) return;
      const overlays = document.querySelectorAll('dialog[open],[aria-modal="true"],[role="menu"]');
      if (Array.from(overlays).some(element => !element.closest("details:not([open]),[hidden],[inert]") && element.getClientRects().length > 0)) return;
      event.preventDefault();
      void run(() => markReading(page.host_epoch, session, row, isUnread(row) ? "read" : "unread"));
    };
    schedule.current = refresh;
    refresh();
    window.addEventListener("focus", focus);
    document.addEventListener("visibilitychange", focus);
    window.addEventListener("keydown", toggle);
    return () => {
      schedule.current = undefined;
      if (frame !== undefined) cancelAnimationFrame(frame);
      window.removeEventListener("focus", focus);
      document.removeEventListener("visibilitychange", focus);
      window.removeEventListener("keydown", toggle);
    };
  }, [session]);
  return null;
}
