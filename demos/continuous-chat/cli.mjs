// A terminal chat that continues across restarts.
//
//   npm run cli -- ada      chat as "ada" (the name picks the conversation)
//   /new   forget this conversation      /quit   leave (come back any time)

import * as readline from "node:readline";
import { createChat } from "./chat.mjs";

const name = process.argv[2] ?? "me";
const chat = createChat({ file: process.env.CHAT_DB ?? "chat.db" });

const earlier = await chat.recent(name);
if (earlier.length > 0) {
  console.log(`--- welcome back, ${name}: the last ${earlier.length} exchanges (${await chat.steps(name)} steps saved) ---`);
  for (const turn of earlier) console.log(`you> ${turn.you}\nbot> ${turn.bot}`);
  console.log("---");
} else {
  console.log(`Chat as "${name}". Say something; /new forgets, /quit leaves.`);
}

const rl = readline.createInterface({ input: process.stdin, output: process.stdout, prompt: "you> " });
rl.prompt();
for await (const raw of rl) {
  const line = raw.trim();
  if (line === "/quit") break;
  if (line === "/new") {
    await chat.forget(name);
    console.log("(conversation forgotten)");
  } else if (line) {
    process.stdout.write("bot> ");
    try {
      await chat.say(name, line, (text) => process.stdout.write(text));
      process.stdout.write("\n");
    } catch (error) {
      console.log(`\n(failed: ${error.code ?? error.name}: ${error.message})`);
    }
  }
  rl.prompt();
}
rl.close();
await chat.close();
