// A highlighted code block (Shiki at build time) with a copy button.
import { highlight } from "@/lib/highlight";
import CopyButton from "./CopyButton";

export default async function Code({ code, lang = "text", copy = true }: { code: string; lang?: string; copy?: boolean }) {
  const html = await highlight(code.replace(/\n$/, ""), lang);
  return (
    <div className="code-block">
      <div className="code-scroll" dangerouslySetInnerHTML={{ __html: html }} />
      {copy && <CopyButton code={code.replace(/\n$/, "")} />}
    </div>
  );
}
