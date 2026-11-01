type Error = Box<dyn std::error::Error>;

pub async fn storage() -> Result<(), Error> {
    // [show]
    use criogen_storage::{OnBusy, Storage};

    // One process: a SQLite file. Tables are created on first use.
    let storage = Storage::connect("sqlite://agent.db").await?;

    // Several processes or machines: Postgres. A thread is used by one run
    // at a time, across every process.
    let storage = Storage::connect("postgres://app:secret@db.internal/agents?sslmode=verify-full")
        .await?
        .on_busy(OnBusy::Wait); // or Reject, or Interrupt the running one

    // Tests: in memory.
    let storage = Storage::in_memory();

    // At shutdown, release the database.
    storage.close().await;
    // [/show]
    Ok(())
}
