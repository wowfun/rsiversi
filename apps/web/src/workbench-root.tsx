import "../styles.css";
import shellCss from "./shell.module.css";
import "../code-highlight.css";
import { createWorkbench } from "./runtime/workbench.js";
import { ResourcePresentations } from "./details/resources.tsx";
import { createRoot } from "react-dom/client";
import { flushSync } from "react-dom";
import { IconSprite } from "./icon-sprite.tsx";
import { FrameCommitObserver, PresentationBoundary } from "./frame-commit.tsx";
import { Shell } from "./shell.tsx";
import { Conversation } from "./conversation.tsx";
import { Resources } from "./resources.tsx";
import { Navigation } from "./navigation.tsx";
import { slots, host, renderer } from "./slots.tsx";

declare module "@rsi/dsh-slots" {
  interface SlotMap {
    root: { kind: "single"; scope: "root" };
    "rsi.navigation": { kind: "single"; scope: "root" };
    "rsi.main": { kind: "single"; scope: "root"; owner: Record<string, never> };
    "rsi.resources": { kind: "single"; scope: "session-maybe" };
  }
}
const registrations = [
  slots.register(
    {
      name: "root",
      registrant: "rsi.workbench",
      children: {
        "rsi.navigation": { kind: "single", scope: "root" },
        "rsi.main": { kind: "single", scope: "root" },
        "rsi.resources": { kind: "single", scope: "session-maybe" },
      },
    },
    Shell,
  ),
  slots.register(
    { name: "rsi.navigation", registrant: "rsi.workbench.navigation" },
    Navigation,
  ),
  slots.register({ name: "rsi.main", registrant: "rsi.gui" }, Conversation),
  slots.register(
    { name: "rsi.resources", registrant: "rsi.session.resources" },
    Resources,
  ),
];
void registrations;
let active = false;
export function mountWorkbench(
  element: HTMLElement,
  options: Parameters<typeof createWorkbench>[0] = {},
) {
  if (active) throw new Error("The document workbench already has an owner");
  active = true;
  element.classList.add(shellCss.scope);
  const root = createRoot(element);
  try {
    flushSync(() =>
      root.render(
        <PresentationBoundary>
          <IconSprite />
          {renderer.renderRoot(host, {})}
          <ResourcePresentations />
          <FrameCommitObserver />
        </PresentationBoundary>,
      ),
    );
    const runtime = createWorkbench(options);
    return {
      runtime,
      async dispose() {
        await runtime.dispose();
        root.unmount();
        active = false;
        element.classList.remove(shellCss.scope);
      },
    };
  } catch (error) {
    root.unmount();
    active = false;
    element.classList.remove(shellCss.scope);
    throw error;
  }
}
