import {
  createElement,
  startTransition,
  useEffect,
  useMemo,
  useState,
  type CSSProperties,
  type ReactNode,
} from "react";
import type { highlightCode } from "../../code-highlight.js";
import { highlights } from "../runtime/highlight-queue.js";
import { writeClipboard } from "/mounts.js";
import { report } from "../bridge.ts";
import {
  IconCheckOutlineRegular,
  IconCopyOutlineRegular,
  IconNowrapFillRegular,
  IconWrapFillRegular,
} from "../../vendor/dsh/dockkit-primitives/icons/index.tsx";

type Element =
  | {
      kind:
        | "paragraph"
        | "quote"
        | "item"
        | "emphasis"
        | "strong"
        | "strike"
        | "span"
        | "table"
        | "table_head"
        | "table_row";
    }
  | { kind: "heading"; level: number }
  | { kind: "list"; start: number | null }
  | { kind: "link"; href: string }
  | {
      kind: "table_cell";
      header: boolean;
      alignment: "left" | "center" | "right" | "none";
    }
  | { kind: "pre"; language?: string | null };
export type MarkdownEvent =
  | { kind: "start"; element: Element }
  | { kind: "end" }
  | { kind: "text" | "code"; text: string }
  | { kind: "break" | "rule" };
type Tree =
  | string
  | {
      element: Element | { kind: "code" | "break" | "rule" };
      children: Tree[];
    };
const tags = {
  paragraph: "p",
  quote: "blockquote",
  item: "li",
  emphasis: "em",
  strong: "strong",
  strike: "s",
  span: "span",
  table: "table",
  table_head: "thead",
  table_row: "tr",
  code: "code",
  break: "br",
  rule: "hr",
} as const;

// This decodes Rust's closed events. Source text is never parsed as Markdown or HTML.
function tree(events: readonly MarkdownEvent[]) {
  const root: Tree[] = [],
    stack = [root];
  for (const event of events) {
    const parent = stack.at(-1)!;
    if (event.kind === "start") {
      const entry = { element: event.element, children: [] as Tree[] };
      parent.push(entry);
      stack.push(entry.children);
    } else if (event.kind === "end") {
      if (stack.length === 1) throw new Error("Unbalanced Markdown");
      stack.pop();
    } else if (event.kind === "text") parent.push(event.text);
    else if (event.kind === "code")
      parent.push({ element: { kind: "code" }, children: [event.text] });
    else if (event.kind === "break" || event.kind === "rule")
      parent.push({ element: { kind: event.kind }, children: [] });
    else throw new Error("Unsupported Markdown event");
  }
  if (stack.length !== 1) throw new Error("Unbalanced Markdown");
  return root;
}
const textOf = (nodes: Tree[]): string =>
  nodes
    .map((node) => (typeof node === "string" ? node : textOf(node.children)))
    .join("");

function CodeBlock({
  text,
  language,
  streaming,
}: {
  text: string;
  language?: string | null;
  streaming: boolean;
}) {
  const [wrapped, setWrapped] = useState(true),
    [copied, setCopied] = useState<string>();
  const [highlighted, setHighlighted] = useState<{
    text: string; language?: string | null; tokens: ReturnType<typeof highlightCode>;
  }>();
  const current = highlighted?.text === text && highlighted.language === language;
  useEffect(() => {
    if (streaming || current) return;
    return highlights.enqueue(text, language, (tokens: ReturnType<typeof highlightCode>) => {
      startTransition(() => setHighlighted({text, language, tokens}));
    });
  }, [text, language, streaming, current]);
  const tokens = !streaming && current
    ? highlighted.tokens : null;
  const copy = async () => {
    try {
      await writeClipboard(text);
      setCopied(text);
    } catch (error) {
      report(error);
    }
  };
  return (
    <div className="code-block">
      <div className="code-toolbar">
        <span className="code-language">{language || "Code"}</span>
        <button
          type="button"
          className="code-wrap"
          aria-label="Wrap lines"
          title={wrapped ? "Unwrap lines" : "Wrap lines"}
          aria-pressed={wrapped}
          onClick={() => setWrapped((value) => !value)}
        >
          {wrapped ? (
            <IconNowrapFillRegular size={14} />
          ) : (
            <IconWrapFillRegular size={14} />
          )}
        </button>
        <button
          type="button"
          className="copy-code"
          aria-label={copied === text ? "Copied" : "Copy code"}
          title={copied === text ? "Copied" : "Copy code"}
          onClick={() => void copy()}
        >
          {copied === text ? (
            <IconCheckOutlineRegular size={14} />
          ) : (
            <IconCopyOutlineRegular size={14} />
          )}
        </button>
      </div>
      <pre className={`transcript-code${wrapped ? " wrap" : ""}`}>
        <code>
          {tokens
            ? tokens.map((line: Record<string, string>[], index: number) => (
                <span key={index}>
                  {line.map((token, part) => {
                    const style: CSSProperties & Record<string, string> = {};
                    for (const theme of ["light", "dark"])
                      if (/^#[0-9a-f]{6,8}$/i.test(token[theme] ?? ""))
                        style[`--code-${theme}`] = token[theme];
                    return (
                      <span key={part} className="code-token" style={style}>
                        {token.content}
                      </span>
                    );
                  })}
                  {index + 1 < tokens.length ? "\n" : ""}
                </span>
              ))
            : text}
        </code>
      </pre>
    </div>
  );
}

function render(nodes: Tree[], streaming: boolean): ReactNode[] {
  return nodes.map((node, key) => {
    if (typeof node === "string") return node;
    const element = node.element,
      children = render(node.children, streaming);
    switch (element.kind) {
      case "pre":
        return (
          <CodeBlock
            key={key}
            text={textOf(node.children)}
            language={element.language}
            streaming={streaming}
          />
        );
      case "heading":
        if (element.level < 1 || element.level > 6)
          throw new Error("Unsupported Markdown heading");
        return createElement(`h${element.level}`, { key }, children);
      case "list":
        return element.start === null ? (
          <ul key={key}>{children}</ul>
        ) : (
          <ol key={key} start={element.start}>
            {children}
          </ol>
        );
      case "link":
        return (
          <a
            key={key}
            href={element.href}
            target="_blank"
            rel="noopener noreferrer"
          >
            {children}
          </a>
        );
      case "table": {
        const header = node.children.filter(
          (child) =>
            typeof child !== "string" && child.element.kind === "table_head",
        );
        const rows = node.children.filter(
          (child) =>
            typeof child === "string" || child.element.kind !== "table_head",
        );
        return (
          <div
            key={key}
            className="markdown-table"
            role="region"
            aria-label="Table"
            tabIndex={0}
          >
            <table>
              {render(header, streaming)}
              <tbody>{render(rows, streaming)}</tbody>
            </table>
          </div>
        );
      }
      case "table_head":
        return (
          <thead key={key}>
            <tr>{children}</tr>
          </thead>
        );
      case "table_cell":
        return element.header ? (
          <th key={key} scope="col" data-align={element.alignment ?? undefined}>
            {children}
          </th>
        ) : (
          <td key={key} data-align={element.alignment ?? undefined}>
            {children}
          </td>
        );
      case "break":
        return <br key={key} />;
      case "rule":
        return <hr key={key} />;
      default: {
        const tag = tags[element.kind];
        if (!tag) throw new Error("Unsupported Markdown element");
        return createElement(tag, { key }, children);
      }
    }
  });
}
export function Markdown({ events, streaming = false }: { events: readonly MarkdownEvent[]; streaming?: boolean }) {
  return useMemo(() => render(tree(events), streaming), [events, streaming]);
}
