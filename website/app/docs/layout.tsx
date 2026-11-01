import type { ReactNode } from "react";
import "@/styles/docs.css";
import DocsFrame from "@/components/docs/DocsFrame";

export default function DocsLayout({ children }: { children: ReactNode }) {
  return <DocsFrame>{children}</DocsFrame>;
}
