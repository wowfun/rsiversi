import {
  createElement,
  startTransition,
  useEffect,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
  type CSSProperties,
  type ReactNode,
} from "react";
import type { HighlightToken } from "../../code-highlight.js";
import { highlights } from "../runtime/highlight-queue.js";
import { CopyButton } from "./copy-button.tsx";
import { MarkdownImage } from "./markdown-image.tsx";
import { TranscriptFileImage } from "./transcript-file-image.tsx";
import { ImageFileLink } from "./image-file-link.tsx";
import linkStyles from "./markdown-image.module.css";
import {
  IconGlobeOutlineMedium,
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
  | { kind: "image"; src: string }
  | { kind: "file_image" | "file_image_link"; path: string }
  | {
      kind: "table_cell";
      header: boolean;
      alignment: "left" | "center" | "right" | "none";
    }
  | { kind: "pre"; language?: string | null };
export type MarkdownEvent =
  | { kind: "start"; element: Element }
  | { kind: "end" }
  | { kind: "text" | "code" | "code_link"; text: string }
  | { kind: "math"; text: string; display: boolean }
  | { kind: "break" | "rule" };
type Tree =
  | string
  | {
      element: Element | { kind: "code" | "break" | "rule" } | {kind:"math"; display:boolean};
      children: Tree[];
    };
function onlyImages(children: Tree[]) {
  return children.length > 0 && children.every(child => typeof child !== "string" &&
    (child.element.kind === "image" || child.element.kind === "file_image"));
}
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
    else if (event.kind === "code" || event.kind === "code_link") {
      const content = event.kind === "code" ? event.text :
        {element: {kind: "link" as const, href: event.text}, children: [event.text]};
      parent.push({element: {kind: "code"}, children: [content]});
    }
    else if (event.kind === "math")
      parent.push({element:{kind:"math",display:event.display},children:[event.text]});
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
  const [wrapped, setWrapped] = useState(true);
  const [highlighted, setHighlighted] = useState<{
    text: string; language?: string | null; tokens: HighlightToken[][] | null;
  }>();
  const current = highlighted?.text === text && highlighted.language === language;
  useEffect(() => {
    if (streaming || current) return;
    return highlights.enqueue(text, language, (tokens: HighlightToken[][] | null) => {
      startTransition(() => setHighlighted({text, language, tokens}));
    });
  }, [text, language, streaming, current]);
  const tokens = !streaming && current
    ? highlighted.tokens : null;
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
        <CopyButton className="copy-code" text={text} label="Copy code" size={14}/>
      </div>
      <pre className={`transcript-code${wrapped ? " wrap" : ""}`}>
        <code>
          {tokens
            ? tokens.map((line, index) => (
                <span key={index}>
                  {line.map((token, part) => {
                    const style: CSSProperties & Record<string, string> = {};
                    for (const theme of ["light", "dark"] as const) {
                      const color = token[theme];
                      if (color && /^#[0-9a-f]{6,8}$/i.test(color))
                        style[`--code-${theme}`] = color;
                    }
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

function Formula({source,display,streaming}: {source:string;display:boolean;streaming:boolean}) {
  const host = useRef<HTMLSpanElement>(null);
  useLayoutEffect(() => {
    const target = host.current!;
    const delimiter = display ? "$$" : "$";
    const literal = delimiter + source + delimiter;
    target.textContent = literal;
    if (streaming) return;
    let current = true;
    void import("/math.js").then(({typeset}) => {
      if (current) typeset(target,source,display);
    }).catch(() => { if (current) target.textContent = literal; });
    return () => { current = false; };
  }, [source,display,streaming]);
  return <span ref={host}/>;
}

function render(nodes: Tree[], streaming: boolean, inQuote = false, inLink = false): ReactNode[] {
  return nodes.map((node, key) => {
    if (typeof node === "string") return node;
    const element = node.element;
    const children = () => render(node.children, streaming, inQuote || element.kind === "quote", inLink || element.kind === "link" || element.kind === "file_image_link");
    switch (element.kind) {
      case "pre":
        if (element.language === "math" && !streaming)
          return <Formula key={key} source={textOf(node.children)} display streaming={false}/>;
        return (
          <CodeBlock
            key={key}
            text={textOf(node.children)}
            language={element.language}
            streaming={streaming}
          />
        );
      case "math":
        return <Formula key={key} source={textOf(node.children)} display={element.display} streaming={streaming}/>;
      case "heading":
        if (element.level < 1 || element.level > 6)
          throw new Error("Unsupported Markdown heading");
        return createElement(`h${element.level}`, { key }, children());
      case "list":
        return element.start === null ? (
          <ul key={key}>{children()}</ul>
        ) : (
          <ol key={key} start={element.start}>
            {children()}
          </ol>
        );
      case "image":
        return <MarkdownImage key={`${key}:${element.src}`} src={element.src} alt={textOf(node.children)} linked={inLink}/>;
      case "file_image":
        return streaming ? textOf(node.children) : <TranscriptFileImage key={`${key}:${element.path}`} path={element.path} alt={textOf(node.children)} linked={inLink}/>;
      case "file_image_link": {
        return streaming ? children() : <ImageFileLink key={`${key}:${element.path}`} path={element.path} imagesOnly={onlyImages(node.children)}>{children()}</ImageFileLink>;
      }
      case "link":
        if (inLink) return children();
        return (
          <a
            key={key}
            className={linkStyles.link}
            href={element.href}
            target="_blank"
            rel="noopener noreferrer"
          >
            {!onlyImages(node.children) && <IconGlobeOutlineMedium className={linkStyles.linkIcon}/>}
            {children()}
          </a>
        );
      case "table": {
        const [header, ...rows] = node.children;
        const wide = !inQuote && typeof header === "object" && header.children.length >= 4;
        return (
          <div
            key={key}
            className={wide ? "markdown-table md-table-wide" : "markdown-table"}
            role="region"
            aria-label="Table"
            tabIndex={wide ? 0 : undefined}
          >
            <table>
              {header && render([header], streaming, inQuote)}
              <tbody>{render(rows, streaming, inQuote)}</tbody>
            </table>
          </div>
        );
      }
      case "table_head":
        return (
          <thead key={key}>
            <tr>{children()}</tr>
          </thead>
        );
      case "table_cell":
        return element.header ? (
          <th key={key} scope="col" data-align={element.alignment ?? undefined}>
            {children()}
          </th>
        ) : (
          <td key={key} data-align={element.alignment ?? undefined}>
            {children()}
          </td>
        );
      case "break":
        return <br key={key} />;
      case "rule":
        return <hr key={key} />;
      default: {
        const tag = tags[element.kind];
        if (!tag) throw new Error("Unsupported Markdown element");
        return createElement(tag, { key }, children());
      }
    }
  });
}
export function Markdown({ events, streaming = false }: { events: readonly MarkdownEvent[]; streaming?: boolean }) {
  return useMemo(() => render(tree(events), streaming), [events, streaming]);
}
