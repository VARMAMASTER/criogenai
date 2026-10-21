// One code sample in every language that has it, as synced tabs: choosing
// Python once shows Python on every page.
import { LANG_ORDER, LANGS, has, snippet, type Lang } from "@/lib/snippets";
import Code from "./Code";
import TabsClient from "./TabsClient";

export default function Sample({ name, region = "", only }: { name: string; region?: string; only?: Lang[] }) {
  const langs = LANG_ORDER.filter((l) => (only ? only.includes(l) : true) && has(l, name));
  if (langs.length === 1) return <Code code={snippet(langs[0], name, region)} lang={LANGS[langs[0]].ext} />;
  return (
    <TabsClient
      sync
      labels={langs.map((l) => LANGS[l].label)}
      panels={langs.map((l) => (
        <Code key={l} code={snippet(l, name, region)} lang={LANGS[l].ext} />
      ))}
    />
  );
}
