import os


async def main() -> None:
    # [show]
    from criogen import Storage

    storage = Storage(
        "postgres://app:secret@db.internal/agents",
        encryption_keys=os.environ["CRIOGEN_ENCRYPTION_KEYS"],  # "k1:<64 hex digits>"
        audit=True,
        retention={"thread_ttl_secs": 30 * 86_400},
    )

    await storage.prune()  # apply retention (e.g. from a cron job)

    # Who did what on a thread (no message text, arguments or results).
    records = await storage.audit_records(thread_id="visit-7f3a")
    report = await storage.verify_audit()  # checks the hash chain
    assert report["ok"]
    # [/show]
