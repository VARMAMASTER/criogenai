type Error = Box<dyn std::error::Error>;

pub async fn compliance() -> Result<(), Error> {
    // [show]
    use std::time::Duration;

    use agentbuild_storage::{AuditQuery, Keyring, Retention, Storage};

    // AGENTBUILD_ENCRYPTION_KEYS="k1:<64 hex digits>"  (openssl rand -hex 32)
    let keys = Keyring::from_env("AGENTBUILD_ENCRYPTION_KEYS")?;
    let storage = Storage::connect_encrypted("postgres://app:secret@db.internal/agents", keys)
        .await?
        .with_audit()
        .retention(Retention::new().thread_ttl(Duration::from_secs(30 * 86_400)));

    // Delete what retention says, once an hour.
    let pruner = storage.spawn_pruner(Duration::from_secs(3_600));

    // Who did what on a thread (no message text, arguments or results).
    let audit = storage.audit().expect("audit is on");
    let records = audit.query(&AuditQuery::new().thread("visit-7f3a")).await?;
    let report = audit.verify().await?; // checks the hash chain
    assert!(report.ok);

    // Key rotation: put the new key first ("k2:…,k1:…"), then
    storage.reencrypt().await?;
    // [/show]
    Ok(())
}
