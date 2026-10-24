// The chat, with the offline model: continuity, separate people, forgetting.
import test from "node:test";
import assert from "node:assert/strict";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { cleanName, createChat, pickModel } from "./chat.mjs";

const fresh = () => {
  const dir = mkdtempSync(join(tmpdir(), "chat-"));
  return { file: join(dir, "chat.db"), dir };
};

test("a conversation continues after the program is closed and opened again", async () => {
  const { file, dir } = fresh();
  const first = createChat({ file, model: pickModel("fake") });
  await first.say("sai", "hello");
  await first.say("sai", "I like tea");
  assert.equal(await first.steps("sai"), 4);
  await first.close();

  const second = createChat({ file, model: pickModel("fake") });
  assert.equal(await second.steps("sai"), 4, "the saved conversation is still there");
  assert.deepEqual((await second.recent("sai")).map((t) => t.you), ["hello", "I like tea"]);
  await second.say("sai", "and again");
  assert.equal(await second.steps("sai"), 6, "it went on from step 4");
  await second.close();
  rmSync(dir, { recursive: true, force: true });
});

test("each name has its own conversation, and forgetting one leaves the other", async () => {
  const { file, dir } = fresh();
  const chat = createChat({ file, model: pickModel("fake") });
  await chat.say("ada", "I am Ada");
  await chat.say("bob", "I am Bob");
  assert.deepEqual((await chat.recent("ada")).map((t) => t.you), ["I am Ada"]);
  await chat.forget("ada");
  assert.equal(await chat.steps("ada"), 0);
  assert.deepEqual(await chat.recent("ada"), []);
  assert.equal(await chat.steps("bob"), 2);
  await chat.close();
  rmSync(dir, { recursive: true, force: true });
});

test("a streamed answer arrives in pieces that add up to the answer", async () => {
  const { file, dir } = fresh();
  const chat = createChat({ file, model: pickModel("fake") });
  const pieces = [];
  const answer = await chat.say("sai", "hi", (piece) => pieces.push(piece));
  assert.ok(pieces.length >= 1);
  assert.equal(pieces.join(""), answer);
  await chat.close();
  rmSync(dir, { recursive: true, force: true });
});

test("names are checked", () => {
  assert.equal(cleanName(" sai_1 "), "sai_1");
  for (const bad of ["", "a b", "../x", "a/b", "x".repeat(41), "é"]) {
    assert.throws(() => cleanName(bad), /letters, digits/, bad);
  }
});
