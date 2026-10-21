// [show]
import { Storage } from "@agentbuild/sdk";

const storage = new Storage({
  url: "postgres://app:secret@db.internal/agents",
  encryptionKeys: process.env.AGENTBUILD_ENCRYPTION_KEYS, // "k1:<64 hex digits>"
  audit: true,
  retention: { threadTtlSecs: 30 * 86_400 },
});

await storage.prune(); // apply retention (e.g. from a cron job)

// Who did what on a thread (no message text, arguments or results).
const records = await storage.auditRecords({ threadId: "visit-7f3a" });
const report = await storage.verifyAudit(); // checks the hash chain
if (!report.ok) throw new Error(`audit chain broken at ${report.first_broken}`);
// [/show]
