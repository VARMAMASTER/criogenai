//! Encryption at rest (design 0008 §15.2): the keys, and how the SQL
//! backends seal and open the columns that hold content.
//!
//! An encrypted column is `enc:v1:<key id>:<base64>`; the bytes are a
//! 16-byte salt, a 12-byte nonce, the ciphertext and a 16-byte tag. Each
//! row's AES-256-GCM key is derived (HKDF-SHA256) from the key and the
//! row's salt, and the additional data names the row, so a row moved into
//! another row's place does not decrypt.

use crate::error::{StorageError, StorageErrorKind};

#[cfg(feature = "encryption")]
pub use keys::{EncryptionKey, Keyring, ReencryptReport};

/// Encrypted text starts with this (JSON text never starts with `e`).
pub(crate) const PREFIX: &str = "enc:v1:";

/// The additional data that names a row: its parts as a JSON list, so
/// parts containing separators cannot be confused.
pub(crate) fn aad(parts: &[&str]) -> String {
    serde_json::to_string(parts).unwrap_or_default()
}

/// A content column that should be sealed is not.
pub(crate) fn plaintext_in_encrypted(what: &str) -> StorageError {
    StorageError::new(
        StorageErrorKind::Corrupt,
        format!(
            "the {what} is not encrypted, but this database is: it was changed outside \
             CriogenAI, or written before encryption was on (`reencrypt` converts it)"
        ),
    )
}

pub(crate) fn key_missing(message: impl Into<String>) -> StorageError {
    StorageError::new(StorageErrorKind::EncryptionKeyMissing, message)
}

/// What the SQL backends use to seal and open columns. Without keys (or
/// without the `encryption` feature) it stores text as it is, and refuses
/// to read encrypted text.
#[derive(Clone, Default)]
pub(crate) struct Sealer {
    #[cfg(feature = "encryption")]
    keys: Option<std::sync::Arc<Keyring>>,
    /// Set only by `reencrypt`, which converts a database that holds
    /// plaintext rows (see [`Sealer::open`]).
    #[cfg(feature = "encryption")]
    migrating: bool,
}

impl std::fmt::Debug for Sealer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Sealer")
            .field("encrypted", &self.is_on())
            .finish()
    }
}

impl Sealer {
    #[cfg(feature = "encryption")]
    pub(crate) fn new(keys: Option<Keyring>) -> Self {
        Self {
            keys: keys.map(std::sync::Arc::new),
            migrating: false,
        }
    }

    /// The same keys, for `reencrypt` only: it reads rows that are still
    /// plaintext in order to seal them.
    #[cfg(feature = "encryption")]
    pub(crate) fn for_migration(&self) -> Self {
        Self {
            keys: self.keys.clone(),
            migrating: true,
        }
    }

    /// `true` if a stored content column without the encryption prefix is
    /// an error: the keys are set, and this is not `reencrypt`. Without it
    /// anyone who can write to the database could plant plaintext rows
    /// (a forged checkpoint, decision or memory fact) that skip the
    /// authentication of every sealed one.
    pub(crate) fn rejects_plaintext(&self) -> bool {
        #[cfg(feature = "encryption")]
        return self.keys.is_some() && !self.migrating;
        #[cfg(not(feature = "encryption"))]
        return false;
    }

    /// `true` if new writes are encrypted.
    pub(crate) fn is_on(&self) -> bool {
        #[cfg(feature = "encryption")]
        return self.keys.is_some();
        #[cfg(not(feature = "encryption"))]
        return false;
    }

    /// `text` as stored: encrypted with the active key when keys are set.
    pub(crate) fn seal(&self, aad: &str, text: String) -> Result<String, StorageError> {
        #[cfg(feature = "encryption")]
        if let Some(keys) = &self.keys {
            return keys.active().seal(aad, &text);
        }
        let _ = aad;
        Ok(text)
    }

    /// [`seal`](Self::seal) for an optional column.
    pub(crate) fn seal_opt(
        &self,
        aad: &str,
        text: Option<String>,
    ) -> Result<Option<String>, StorageError> {
        text.map(|text| self.seal(aad, text)).transpose()
    }

    /// The plain text of a stored column. `what` names it in errors.
    pub(crate) fn open(
        &self,
        aad: &str,
        stored: String,
        what: &str,
    ) -> Result<String, StorageError> {
        let Some(rest) = stored.strip_prefix(PREFIX) else {
            if self.rejects_plaintext() {
                return Err(plaintext_in_encrypted(what));
            }
            return Ok(stored);
        };
        let id = rest.split(':').next().unwrap_or_default();
        #[cfg(feature = "encryption")]
        if let Some(keys) = &self.keys {
            let Some(key) = keys.get(id) else {
                return Err(key_missing(format!(
                    "the {what} is encrypted with key `{id}`, which is not in the keyring"
                )));
            };
            return key.open(aad, rest, what);
        }
        let _ = aad;
        Err(key_missing(format!(
            "the {what} is encrypted (key `{id}`); open the storage with its keys"
        )))
    }

    /// `enc:v1:<active id>:`, with keys.
    #[cfg(feature = "encryption")]
    pub(crate) fn active_prefix(&self) -> Option<String> {
        self.keys
            .as_ref()
            .map(|keys| format!("{PREFIX}{}:", keys.active().id()))
    }

    /// How a memory key may be stored: with keys, its tag under each key
    /// (the active key's first), then the key itself (rows written before
    /// encryption was on). Without keys, the key itself.
    pub(crate) fn memory_keys(&self, namespace: &str, key: &str) -> Vec<String> {
        #[cfg(feature = "encryption")]
        if let Some(keys) = &self.keys {
            let mut all: Vec<String> = keys
                .keys
                .iter()
                .map(|k| k.memory_tag(namespace, key))
                .collect();
            all.push(key.to_string());
            return all;
        }
        let _ = namespace;
        vec![key.to_string()]
    }

    /// Checks the database's encryption settings against the keys: `stored`
    /// is the `criogen_settings` table. Returns the rows to add (the
    /// `encryption` flag and a check value per new key id).
    pub(crate) fn check_settings(
        &self,
        stored: &std::collections::HashMap<String, String>,
    ) -> Result<Vec<(String, String)>, StorageError> {
        let encrypted = stored.get("encryption").is_some_and(|v| v == "on");
        #[cfg(feature = "encryption")]
        if let Some(keys) = &self.keys {
            let mut add = Vec::new();
            if !encrypted {
                add.push(("encryption".to_string(), "on".to_string()));
            }
            for key in &keys.keys {
                let name = format!("key/{}", key.id());
                let aad = aad(&["settings", &name]);
                match stored.get(&name) {
                    Some(check) => {
                        let opened = check
                            .strip_prefix(PREFIX)
                            .and_then(|rest| key.open(&aad, rest, "key check").ok());
                        if opened.as_deref() != Some(KEY_CHECK) {
                            return Err(StorageError::new(
                                StorageErrorKind::EncryptionKeyInvalid,
                                format!(
                                    "key `{}` is not the key this database knows by that id",
                                    key.id()
                                ),
                            ));
                        }
                    }
                    None => add.push((name, key.seal(&aad, KEY_CHECK)?)),
                }
            }
            return Ok(add);
        }
        if encrypted {
            #[cfg(feature = "encryption")]
            let how = "open it with its keys (`Storage::connect_encrypted`)";
            #[cfg(not(feature = "encryption"))]
            let how = "open it with its keys (feature `encryption`)";
            return Err(key_missing(format!("this database is encrypted: {how}")));
        }
        Ok(Vec::new())
    }
}

/// What a key check value holds.
#[cfg(feature = "encryption")]
const KEY_CHECK: &str = "criogenai key check";

#[cfg(feature = "encryption")]
mod keys {
    use std::fmt;

    use aws_lc_rs::aead::{AES_256_GCM, Aad, Nonce, RandomizedNonceKey};
    use aws_lc_rs::{hkdf, hmac, rand};
    use base64::Engine;
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;

    use super::{PREFIX, aad, key_missing};
    use crate::error::{StorageError, StorageErrorKind};

    const SALT_LEN: usize = 16;
    const NONCE_LEN: usize = 12;
    const TAG_LEN: usize = 16;

    /// HKDF output of 32 bytes (an AES-256 key).
    struct Len32;

    impl hkdf::KeyType for Len32 {
        fn len(&self) -> usize {
            32
        }
    }

    /// One 32-byte encryption key and its id. The bytes are never printed.
    pub struct EncryptionKey {
        id: String,
        /// Derives each row's AES-256-GCM key.
        rows: hkdf::Prk,
        /// Tags memory keys.
        index: hmac::Key,
    }

    impl fmt::Debug for EncryptionKey {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.debug_struct("EncryptionKey")
                .field("id", &self.id)
                .finish_non_exhaustive()
        }
    }

    fn invalid(message: impl Into<String>) -> StorageError {
        StorageError::new(StorageErrorKind::InvalidValue, message)
    }

    impl EncryptionKey {
        /// A key from 32 bytes (for example from a key management service).
        /// `id` names it in stored rows: 1–32 characters of `A-Z a-z 0-9 _
        /// . -`.
        pub fn new(id: impl Into<String>, bytes: &[u8]) -> Result<Self, StorageError> {
            let id = id.into();
            if id.is_empty()
                || id.len() > 32
                || !id
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-'))
            {
                return Err(invalid(format!(
                    "invalid key id {id:?}: 1 to 32 characters of A-Z a-z 0-9 _ . -"
                )));
            }
            if bytes.len() != 32 {
                return Err(invalid(format!(
                    "key `{id}` has {} bytes; encryption keys have 32",
                    bytes.len()
                )));
            }
            let master = hkdf::Salt::new(hkdf::HKDF_SHA256, b"criogen/v1").extract(bytes);
            let index = master
                .expand(&[b"memory keys"], hmac::HMAC_SHA256)
                .map(hmac::Key::from)
                .map_err(|_| invalid("cannot derive the key"))?;
            let mut rows = [0u8; 32];
            master
                .expand(&[b"rows"], Len32)
                .and_then(|okm| okm.fill(&mut rows))
                .map_err(|_| invalid("cannot derive the key"))?;
            Ok(Self {
                id,
                rows: hkdf::Salt::new(hkdf::HKDF_SHA256, b"criogen/v1/rows").extract(&rows),
                index,
            })
        }

        /// A key from 64 hex digits (`openssl rand -hex 32` makes one).
        pub fn from_hex(id: impl Into<String>, hex: &str) -> Result<Self, StorageError> {
            let id = id.into();
            let hex = hex.trim();
            let digits: Option<Vec<u8>> = (0..hex.len())
                .step_by(2)
                .map(|i| {
                    hex.get(i..i + 2)
                        .and_then(|pair| u8::from_str_radix(pair, 16).ok())
                })
                .collect();
            match digits {
                Some(bytes) if hex.len() == 64 => Self::new(id, &bytes),
                _ => Err(invalid(format!(
                    "key `{id}` is not 64 hex digits (32 bytes)"
                ))),
            }
        }

        /// The key's id.
        pub fn id(&self) -> &str {
            &self.id
        }

        /// The AES-256-GCM key for one row's salt.
        fn row_key(&self, salt: &[u8]) -> Result<RandomizedNonceKey, StorageError> {
            let mut key = [0u8; 32];
            self.rows
                .expand(&[b"row", salt], Len32)
                .and_then(|okm| okm.fill(&mut key))
                .and_then(|()| RandomizedNonceKey::new(&AES_256_GCM, &key))
                .map_err(|_| crate::error::corrupt("cannot derive a row key"))
        }

        /// `enc:v1:<id>:<base64 of salt, nonce, ciphertext and tag>`.
        pub(crate) fn seal(&self, aad: &str, text: &str) -> Result<String, StorageError> {
            let mut salt = [0u8; SALT_LEN];
            rand::fill(&mut salt).map_err(|_| crate::error::corrupt("no random bytes"))?;
            let key = self.row_key(&salt)?;
            let mut data = text.as_bytes().to_vec();
            let nonce = key
                .seal_in_place_append_tag(Aad::from(aad.as_bytes()), &mut data)
                .map_err(|_| crate::error::corrupt("encryption failed"))?;
            let mut out = Vec::with_capacity(SALT_LEN + NONCE_LEN + data.len());
            out.extend_from_slice(&salt);
            out.extend_from_slice(nonce.as_ref());
            out.extend_from_slice(&data);
            Ok(format!(
                "{PREFIX}{}:{}",
                self.id,
                URL_SAFE_NO_PAD.encode(out)
            ))
        }

        /// Opens `rest`: the stored text after `enc:v1:` (`<id>:<base64>`).
        pub(crate) fn open(
            &self,
            aad: &str,
            rest: &str,
            what: &str,
        ) -> Result<String, StorageError> {
            let unreadable = || {
                crate::error::corrupt(format!(
                    "the {what} does not decrypt with key `{}`: it was changed, or moved from \
                     another row",
                    self.id
                ))
            };
            let encoded = rest
                .strip_prefix(self.id.as_str())
                .and_then(|r| r.strip_prefix(':'))
                .ok_or_else(unreadable)?;
            let bytes = URL_SAFE_NO_PAD.decode(encoded).map_err(|_| unreadable())?;
            if bytes.len() < SALT_LEN + NONCE_LEN + TAG_LEN {
                return Err(unreadable());
            }
            let (salt, rest) = bytes.split_at(SALT_LEN);
            let (nonce, sealed) = rest.split_at(NONCE_LEN);
            let key = self.row_key(salt)?;
            let nonce = Nonce::try_assume_unique_for_key(nonce).map_err(|_| unreadable())?;
            let mut data = sealed.to_vec();
            let plain = key
                .open_in_place(nonce, Aad::from(aad.as_bytes()), &mut data)
                .map_err(|_| unreadable())?;
            String::from_utf8(plain.to_vec()).map_err(|_| unreadable())
        }

        /// How a memory key is stored under this key: `~` and an HMAC of
        /// the namespace and key.
        pub(crate) fn memory_tag(&self, namespace: &str, key: &str) -> String {
            let tag = hmac::sign(&self.index, aad(&[namespace, key]).as_bytes());
            format!("~{}", URL_SAFE_NO_PAD.encode(tag.as_ref()))
        }
    }

    /// The keys of encrypted storage: the active key, which encrypts new
    /// writes, and older keys that still decrypt rows written with them
    /// (design 0008 §15.2). Rotate by putting a new key first; after
    /// `reencrypt`, older keys can be dropped.
    pub struct Keyring {
        pub(super) keys: Vec<EncryptionKey>,
    }

    impl fmt::Debug for Keyring {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.debug_struct("Keyring").field("ids", &self.ids()).finish()
        }
    }

    impl Keyring {
        /// A keyring with one key.
        pub fn new(active: EncryptionKey) -> Self {
            Self { keys: vec![active] }
        }

        /// Adds an older key: it decrypts rows written with it, and is never
        /// used for new writes. Fails with `invalid_value` if the id is taken.
        pub fn with_previous(mut self, key: EncryptionKey) -> Result<Self, StorageError> {
            if self.get(key.id()).is_some() {
                return Err(invalid(format!("key id `{}` is used twice", key.id())));
            }
            self.keys.push(key);
            Ok(self)
        }

        /// Keys from text such as `k2:<64 hex digits>,k1:<64 hex digits>`:
        /// the first is active. Whitespace around entries is ignored.
        pub fn parse(text: &str) -> Result<Self, StorageError> {
            let mut keys = text
                .split(',')
                .map(str::trim)
                .filter(|entry| !entry.is_empty())
                .map(|entry| match entry.split_once(':') {
                    Some((id, hex)) => EncryptionKey::from_hex(id.trim(), hex),
                    None => Err(invalid(
                        "each key is `<id>:<64 hex digits>`, separated by commas",
                    )),
                });
            let Some(first) = keys.next() else {
                return Err(key_missing("no encryption keys given"));
            };
            keys.try_fold(Self::new(first?), |ring, key| ring.with_previous(key?))
        }

        /// [`parse`](Self::parse) of the environment variable `name`. Fails
        /// with `encryption_key_missing` if it is not set.
        pub fn from_env(name: &str) -> Result<Self, StorageError> {
            match std::env::var(name) {
                Ok(text) => Self::parse(&text),
                Err(_) => Err(key_missing(format!(
                    "the environment variable `{name}` with the encryption keys is not set"
                ))),
            }
        }

        /// The id of the key that encrypts new writes.
        pub fn active_id(&self) -> &str {
            self.active().id()
        }

        /// Every key id, the active key's first.
        pub fn ids(&self) -> Vec<&str> {
            self.keys.iter().map(EncryptionKey::id).collect()
        }

        pub(crate) fn active(&self) -> &EncryptionKey {
            &self.keys[0]
        }

        pub(crate) fn get(&self, id: &str) -> Option<&EncryptionKey> {
            self.keys.iter().find(|key| key.id() == id)
        }
    }

    /// What `reencrypt` rewrote: rows that were plain or used an older key.
    #[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
    #[non_exhaustive]
    pub struct ReencryptReport {
        /// Checkpoint columns (bodies and snapshots).
        pub checkpoints: u64,
        /// Task writes.
        pub task_writes: u64,
        /// Memory items.
        pub memory_items: u64,
    }
}

#[cfg(all(test, feature = "encryption"))]
mod tests {
    use std::collections::HashMap;

    use super::*;

    fn key(id: &str, byte: u8) -> EncryptionKey {
        EncryptionKey::new(id, &[byte; 32]).unwrap()
    }

    #[test]
    fn sealed_text_opens_only_with_its_key_and_row() {
        let sealer = Sealer::new(Some(Keyring::new(key("k1", 1))));
        let row = aad(&["checkpoint", "t", "c1", "body"]);
        let sealed = sealer.seal(&row, "{\"secret\":1}".into()).unwrap();
        assert!(sealed.starts_with("enc:v1:k1:"), "{sealed}");
        assert!(!sealed.contains("secret"));
        // A new salt and nonce every time.
        assert_ne!(sealer.seal(&row, "{\"secret\":1}".into()).unwrap(), sealed);
        assert_eq!(
            sealer.open(&row, sealed.clone(), "body").unwrap(),
            "{\"secret\":1}"
        );

        // Another row's place: refused.
        let other = aad(&["checkpoint", "t", "c2", "body"]);
        let err = sealer.open(&other, sealed.clone(), "body").unwrap_err();
        assert_eq!(err.code(), "storage_corrupt");
        // A changed byte: refused.
        let mut changed = sealed.clone().into_bytes();
        let last = changed.len() - 2;
        changed[last] = if changed[last] == b'A' { b'B' } else { b'A' };
        let err = sealer
            .open(&row, String::from_utf8(changed).unwrap(), "body")
            .unwrap_err();
        assert_eq!(err.code(), "storage_corrupt");
        // Same id, other bytes: refused.
        let impostor = Sealer::new(Some(Keyring::new(key("k1", 2))));
        assert_eq!(
            impostor
                .open(&row, sealed.clone(), "body")
                .unwrap_err()
                .code(),
            "storage_corrupt"
        );
        // An id not in the keyring, or no keys at all.
        let other_ring = Sealer::new(Some(Keyring::new(key("k2", 1))));
        let err = other_ring.open(&row, sealed.clone(), "body").unwrap_err();
        assert_eq!(err.code(), "encryption_key_missing");
        assert!(err.message.contains("`k1`"), "{}", err.message);
        let plain = Sealer::default();
        assert_eq!(
            plain.open(&row, sealed, "body").unwrap_err().code(),
            "encryption_key_missing"
        );
        // Plain text reads as it is without keys. With keys it is refused:
        // a planted plaintext row would skip every check on a sealed one.
        // Only `reencrypt` reads it, to seal it.
        assert_eq!(plain.open(&row, "[1]".into(), "body").unwrap(), "[1]");
        assert_eq!(plain.seal(&row, "[1]".into()).unwrap(), "[1]");
        let err = sealer.open(&row, "[1]".into(), "body").unwrap_err();
        assert_eq!(err.code(), "storage_corrupt");
        assert!(err.message.contains("reencrypt"), "{}", err.message);
        assert_eq!(
            sealer
                .for_migration()
                .open(&row, "[1]".into(), "body")
                .unwrap(),
            "[1]"
        );
    }

    #[test]
    fn older_keys_open_and_the_active_key_seals() {
        let old = Sealer::new(Some(Keyring::new(key("k1", 1))));
        let row = aad(&["memory", "users/1", "x"]);
        let sealed = old.seal(&row, "\"v\"".into()).unwrap();
        let ring = Keyring::new(key("k2", 2))
            .with_previous(key("k1", 1))
            .unwrap();
        assert_eq!(ring.ids(), ["k2", "k1"]);
        let rotated = Sealer::new(Some(ring));
        assert_eq!(
            rotated.open(&row, sealed.clone(), "value").unwrap(),
            "\"v\""
        );
        let active = rotated.active_prefix().unwrap();
        assert_eq!(active, "enc:v1:k2:");
        assert!(!sealed.starts_with(&active));
        let resealed = rotated.seal(&row, "\"v\"".into()).unwrap();
        assert!(resealed.starts_with(&active));
    }

    #[test]
    fn memory_keys_are_tags_per_key_then_the_key() {
        let ring = Keyring::new(key("k2", 2))
            .with_previous(key("k1", 1))
            .unwrap();
        let sealer = Sealer::new(Some(ring));
        let keys = sealer.memory_keys("users/1", "allergy_penicillin");
        assert_eq!(keys.len(), 3);
        assert!(keys[0].starts_with('~') && keys[1].starts_with('~'));
        assert_ne!(keys[0], keys[1]);
        assert_eq!(keys[2], "allergy_penicillin");
        assert!(!keys[0].contains("allergy"));
        // Deterministic, and bound to the namespace.
        assert_eq!(
            sealer.memory_keys("users/1", "allergy_penicillin")[0],
            keys[0]
        );
        assert_ne!(
            sealer.memory_keys("users/2", "allergy_penicillin")[0],
            keys[0]
        );
        assert_eq!(Sealer::default().memory_keys("users/1", "k"), ["k"]);
    }

    #[test]
    fn keys_parse_from_text_and_never_print() {
        let hex1 = "01".repeat(32);
        let hex2 = "ab".repeat(32);
        let ring = Keyring::parse(&format!(" k2:{hex2} , k1:{hex1} ")).unwrap();
        assert_eq!(ring.active_id(), "k2");
        assert_eq!(ring.ids(), ["k2", "k1"]);
        let shown = format!("{ring:?} {:?}", ring.active());
        assert!(
            !shown.contains("abab") && !shown.contains("0101"),
            "{shown}"
        );

        for bad in [
            format!("k1:{}", "0".repeat(63)),
            format!("k1:{}", "zz".repeat(32)),
            format!("bad id:{hex1}"),
            hex1.clone(),
            format!("k1:{hex1},k1:{hex2}"),
        ] {
            let err = Keyring::parse(&bad).unwrap_err();
            assert_eq!(err.code(), "invalid_value", "{bad}");
            assert!(!err.message.contains(&hex1[..8]), "{}", err.message);
        }
        assert_eq!(
            Keyring::parse(" , ").unwrap_err().code(),
            "encryption_key_missing"
        );
        assert_eq!(
            Keyring::from_env("CRIOGEN_TEST_NO_SUCH_VARIABLE")
                .unwrap_err()
                .code(),
            "encryption_key_missing"
        );
        assert!(EncryptionKey::new("k", &[0; 16]).is_err());
    }

    #[test]
    fn settings_record_the_keys_and_refuse_a_wrong_or_missing_one() {
        let sealer = Sealer::new(Some(Keyring::new(key("k1", 1))));
        let add = sealer.check_settings(&HashMap::new()).unwrap();
        let names: Vec<&str> = add.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, ["encryption", "key/k1"]);
        let stored: HashMap<String, String> = add.into_iter().collect();
        assert!(sealer.check_settings(&stored).unwrap().is_empty());

        let wrong = Sealer::new(Some(Keyring::new(key("k1", 9))));
        assert_eq!(
            wrong.check_settings(&stored).unwrap_err().code(),
            "encryption_key_invalid"
        );
        assert_eq!(
            Sealer::default()
                .check_settings(&stored)
                .unwrap_err()
                .code(),
            "encryption_key_missing"
        );
        assert!(
            Sealer::default()
                .check_settings(&HashMap::new())
                .unwrap()
                .is_empty()
        );
        // A new key is recorded next to the known one.
        let rotated = Sealer::new(Some(
            Keyring::new(key("k2", 2))
                .with_previous(key("k1", 1))
                .unwrap(),
        ));
        let add = rotated.check_settings(&stored).unwrap();
        assert_eq!(add.len(), 1);
        assert_eq!(add[0].0, "key/k2");
    }
}
