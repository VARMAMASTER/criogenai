// The components MDX pages use, by name (the Astro site's Starlight
// components and its own, rebuilt in React).
import Link from "next/link";
import { isValidElement, type ComponentProps, type ReactNode } from "react";
import Aside from "./Aside";
import { CardGrid, LinkCard } from "./Cards";
import Code from "./Code";
import ErrorTable from "./ErrorTable";
import Sample from "./Sample";
import { TabItem, Tabs } from "./Tabs";

function textOf(node: ReactNode): string {
  if (typeof node === "string" || typeof node === "number") return String(node);
  if (Array.isArray(node)) return node.map(textOf).join("");
  if (isValidElement(node)) return textOf((node.props as { children?: ReactNode }).children);
  return "";
}

function Pre({ children }: { children?: ReactNode }) {
  if (isValidElement(children)) {
    const props = children.props as { className?: string; children?: ReactNode };
    const lang = /language-(\S+)/.exec(props.className ?? "")?.[1] ?? "text";
    return <Code code={textOf(props.children)} lang={lang} />;
  }
  return <pre>{children}</pre>;
}

function Table(props: ComponentProps<"table">) {
  return (
    <div className="table-wrap">
      <table {...props} />
    </div>
  );
}

// Links to other pages of the site move without reloading the page.
function A({ href = "", ...props }: ComponentProps<"a">) {
  if (href.startsWith("/") && !href.startsWith("/api/") && !/\.\w+$/.test(href.split("#")[0])) return <Link href={href} {...props} />;
  return <a href={href} {...props} />;
}

export const mdxComponents = {
  a: A,
  pre: Pre,
  table: Table,
  Aside,
  CardGrid,
  LinkCard,
  ErrorTable,
  Sample,
  Tabs,
  TabItem,
};
