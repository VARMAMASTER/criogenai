// The retrieval guide's samples (content/docs/guides/retrieval.mdx), design 0025.
import { Agent, Collection, Embedder, Storage, openai } from "criogenai";

export async function basics(storage: Storage) {
  // [show]
  const handbook = await Collection.open("handbook", { embedder: Embedder.openai(), storage });
  await handbook.add([
    { id: "leave", text: "Annual leave is 25 days a year.", metadata: { section: "hr" } },
    { id: "expenses", text: "Expenses are paid within 30 days.", metadata: { section: "finance" } },
  ]);

  const hits = await handbook.search("How many vacation days?", { k: 3 });
  const hrOnly = await handbook.search("deadlines", { k: 3, where: { section: "hr" } });

  const agent = new Agent({
    model: openai("gpt-5-mini"),
    instructions: "Answer from the handbook; say which section you used.",
    tools: [handbook.asTool("search_handbook", "Search the staff handbook.")],
  });
  console.log((await agent.run("How much leave do I get?")).text);
  // [/show]
  return { hits, hrOnly };
}
