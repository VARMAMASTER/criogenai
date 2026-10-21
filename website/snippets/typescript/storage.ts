// [show]
import { Storage } from "@agentbuild/sdk";

// One process: a SQLite file. Tables are created on first use.
let storage = new Storage("sqlite:///agent.db");

// Several processes or machines: Postgres. A thread is used by one run at a
// time, across every process.
storage = new Storage({ url: "postgres://app:secret@db.internal/agents", onBusy: "wait" });

// Tests: in memory.
storage = new Storage();

// At shutdown, release the database.
await storage.close();
// [/show]
