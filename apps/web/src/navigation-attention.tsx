import { Button } from "./button.tsx";
import { input, run, type Navigation } from "./bridge.ts";
export function AttentionNavigation({ nav }: { nav: Navigation | undefined }) {
  return (
    <section className="attention-navigation" aria-label="Needs attention">
      {nav?.attention_notice && (
        <p role="status" className="hint">
          {nav.attention_notice}
        </p>
      )}
      {nav?.attention?.entries.map((entry) => (
        <div
          className="attention-row"
          key={`${entry.position.conversation.kind}:${entry.position.conversation.id}`}
        >
          <button
            className="nav-item"
            onClick={() =>
              run(() =>
                input.open({
                  action: "attention_open",
                  position: entry.position,
                  target: null,
                }),
              )
            }
          >
            <strong>
              {entry.status === "waiting"
                ? "Waiting for you"
                : entry.status === "unread"
                  ? "New activity"
                  : entry.status === "running"
                    ? "Running"
                    : "Unknown"}
            </strong>
            <small>{entry.position.conversation.id}</small>
          </button>
          {entry.targets.map((target, index) => (
            <Button
              size="sm"
              key={index}
              onClick={() =>
                run(() =>
                  input.open({
                    action: "attention_open",
                    position: entry.position,
                    target,
                  }),
                )
              }
            >
              {target.kind === "native" && target.request.kind === "question"
                ? "Answer question"
                : "Review permission"}{" "}
              {index + 1}
            </Button>
          ))}
        </div>
      ))}
      {nav?.attention?.truncated && (
        <p className="hint">
          Only the bounded active-conversation window is shown.
        </p>
      )}
      {nav?.attention && !nav.attention.entries.length && (
        <p className="hint">
          No conversations in the current attention window.
        </p>
      )}
    </section>
  );
}
