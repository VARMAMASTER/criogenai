async def main() -> None:
    # [show]
    from criogen import Storage

    # One process: a SQLite file. Tables are created on first use.
    storage = Storage("sqlite://agent.db")

    # Several processes or machines: Postgres. A thread is used by one run
    # at a time, across every process.
    storage = Storage("postgres://app:secret@db.internal/agents", on_busy="wait")

    # Tests: in memory.
    storage = Storage()

    # At shutdown, release the database.
    await storage.close()
    # [/show]
