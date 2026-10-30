import {
  Component,
  type ReactNode,
  useLayoutEffect,
  useSyncExternalStore,
} from "react";
import { flushSync } from "react-dom";
import { observable } from "./bridge.ts";
import { FrameTicket } from "./runtime/frame-ticket.js";

const requested = observable<FrameTicket | undefined>(undefined);
let mounted = false;
let failOwner: ((error: Error) => void) | undefined;
const failure = (value: unknown) =>
  value instanceof Error ? value : new Error(String(value));

export function failPresentation(value: unknown) {
  const error = failure(value);
  requested.getSnapshot()?.abort(error);
  failOwner?.(error);
}

export class PresentationBoundary extends Component<
  { children: ReactNode },
  { failed: boolean }
> {
  state = { failed: false };
  static getDerivedStateFromError() {
    return { failed: true };
  }
  componentDidCatch(error: Error) {
    failPresentation(error);
  }
  render() {
    return this.state.failed ? (
      <p role="alert">
        The workbench could not render. Reload this window to reconnect.
      </p>
    ) : (
      this.props.children
    );
  }
}

export function FrameCommitObserver() {
  const ticket = useSyncExternalStore(
    requested.subscribe,
    requested.getSnapshot,
  );
  useLayoutEffect(() => {
    mounted = true;
    return () => {
      mounted = false;
      failPresentation(new Error("Presentation root unmounted"));
    };
  }, []);
  useLayoutEffect(() => {
    ticket?.commit(ticket.epoch, ticket.frameId);
  }, [ticket]);
  return null;
}

// Components remain keyed by their durable identities. Only this observer sees
// frame tickets; flushSync includes all store notifications in the same commit.
export const reactPresentation = {
  begin(epoch: object, frameId: string, failed: (error: Error) => void) {
    requested.getSnapshot()?.abort(new Error("Presentation was replaced"));
    if (!mounted) throw new Error("Presentation root is unavailable");
    failOwner = failed;
    return new FrameTicket(epoch, frameId, failed);
  },
  commit(ticket: FrameTicket, render: () => void, hosts: () => HTMLElement[]) {
    flushSync(() => {
      render();
      ticket.requireHosts(hosts);
      requested.set(ticket);
    });
    return ticket.committed;
  },
  retire() {
    requested.getSnapshot()?.abort(new Error("Presentation owner retired"));
    failOwner = undefined;
    requested.set(undefined);
  },
};
