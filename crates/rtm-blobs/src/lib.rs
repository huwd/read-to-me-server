// SPDX-License-Identifier: AGPL-3.0-or-later
//! Content-addressed blob storage (ADR-0008): immutable blobs on the local filesystem,
//! laid out as `<root>/<kind>/<first 2 hex>/<key>`.
//!
//! Writes are atomic (temporary file, fsync, rename, fsync the directory) and a blob
//! that already exists is never rewritten, so stages can be re-run safely. Reference
//! counts, garbage collection and other backends come later.

use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use rtm_domain::BlobKey;

/// What a blob holds. Each kind has its own directory and key derivation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlobKind {
    Source,
}

impl BlobKind {
    fn as_str(self) -> &'static str {
        match self {
            BlobKind::Source => "source",
        }
    }
}

/// BLAKE3 over a canonical encoding of `fields`: a domain tag naming the kind, then each
/// field as its length (u64, little-endian) followed by its bytes. Length prefixes stop
/// two different field lists from encoding to the same bytes.
fn derive(kind: BlobKind, fields: &[&[u8]]) -> BlobKey {
    let mut hasher = blake3::Hasher::new();
    let tag = format!("read-to-me/blob/{}/v1", kind.as_str());
    for field in std::iter::once(tag.as_bytes()).chain(fields.iter().copied()) {
        hasher.update(&(field.len() as u64).to_le_bytes());
        hasher.update(field);
    }
    BlobKey::from_hash(*hasher.finalize().as_bytes())
}

/// The key for a fetched source (ADR-0008): its bytes and the owner salt, which is empty
/// for public URL-fetched content so household members share downstream work.
pub fn source_key(bytes: &[u8], owner_salt: &[u8]) -> BlobKey {
    derive(BlobKind::Source, &[bytes, owner_salt])
}

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Blobs on the local filesystem under one root directory.
#[derive(Debug, Clone)]
pub struct BlobStore {
    root: PathBuf,
}

impl BlobStore {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    fn path(&self, kind: BlobKind, key: &BlobKey) -> PathBuf {
        // Keys are validated hex, so they are safe as path components.
        self.root
            .join(kind.as_str())
            .join(&key.as_str()[..2])
            .join(key.as_str())
    }

    /// Store `bytes` under `key`, unless a blob with that key already exists.
    pub async fn put(&self, kind: BlobKind, key: &BlobKey, bytes: Vec<u8>) -> io::Result<()> {
        let path = self.path(kind, key);
        let tmp_dir = self.root.join("tmp");
        tokio::task::spawn_blocking(move || write_atomic(&path, &tmp_dir, &bytes))
            .await
            .map_err(io::Error::other)?
    }

    /// Store a fetched source and return its key.
    pub async fn put_source(&self, bytes: Vec<u8>, owner_salt: &[u8]) -> io::Result<BlobKey> {
        let key = source_key(&bytes, owner_salt);
        self.put(BlobKind::Source, &key, bytes).await?;
        Ok(key)
    }

    pub async fn get(&self, kind: BlobKind, key: &BlobKey) -> io::Result<Vec<u8>> {
        tokio::fs::read(self.path(kind, key)).await
    }

    pub async fn exists(&self, kind: BlobKind, key: &BlobKey) -> io::Result<bool> {
        tokio::fs::try_exists(self.path(kind, key)).await
    }
}

fn sync_dir(dir: &Path) -> io::Result<()> {
    File::open(dir)?.sync_all()
}

/// Create `dir` and any missing ancestors, fsyncing the parent of each directory
/// created so the new entries survive a power failure. `create_dir_all` alone leaves
/// them in the page cache: a blob could be reported stored, referenced from the
/// database, and then lose its directory on recovery.
fn create_dir_durable(dir: &Path) -> io::Result<()> {
    let mut missing = Vec::new();
    let mut current = Some(dir);
    while let Some(d) = current {
        if d.as_os_str().is_empty() || d.is_dir() {
            break;
        }
        missing.push(d);
        current = d.parent();
    }
    for d in missing.into_iter().rev() {
        match fs::create_dir(d) {
            Ok(()) => {}
            // Another writer got there first; its parent fsync may not have happened
            // yet, so sync it here too.
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e),
        }
        match d.parent().filter(|p| !p.as_os_str().is_empty()) {
            Some(parent) => sync_dir(parent)?,
            None => sync_dir(Path::new("."))?,
        }
    }
    Ok(())
}

fn write_atomic(path: &Path, tmp_dir: &Path, bytes: &[u8]) -> io::Result<()> {
    let dir = path.parent().expect("blob paths have a parent");
    create_dir_durable(dir)?;
    if path.exists() {
        // Immutable: same key, same content. But an earlier attempt may have crashed
        // between its rename and its fsyncs, so make sure this copy is durable before
        // reporting it stored.
        File::open(path)?.sync_all()?;
        return sync_dir(dir);
    }
    create_dir_durable(tmp_dir)?;
    let tmp = tmp_dir.join(format!(
        "{}.{}.{}",
        path.file_name().and_then(|n| n.to_str()).unwrap_or("blob"),
        std::process::id(),
        TEMP_COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    let result = (|| {
        let mut file = File::create(&tmp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        fs::rename(&tmp, path)?;
        // Make the rename itself durable.
        sync_dir(dir)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn round_trip_and_layout() {
        let dir = tempfile::tempdir().unwrap();
        let store = BlobStore::new(dir.path());
        let key = store.put_source(b"<p>hi</p>".to_vec(), b"").await.unwrap();
        assert_eq!(
            store.get(BlobKind::Source, &key).await.unwrap(),
            b"<p>hi</p>"
        );
        let expected = dir
            .path()
            .join("source")
            .join(&key.as_str()[..2])
            .join(key.as_str());
        assert!(expected.is_file());
        // No temporary files are left behind.
        assert_eq!(fs::read_dir(dir.path().join("tmp")).unwrap().count(), 0);
    }

    #[tokio::test]
    async fn existing_blobs_are_never_rewritten() {
        let dir = tempfile::tempdir().unwrap();
        let store = BlobStore::new(dir.path());
        let key = source_key(b"original", b"");
        store
            .put(BlobKind::Source, &key, b"original".to_vec())
            .await
            .unwrap();
        store
            .put(BlobKind::Source, &key, b"different".to_vec())
            .await
            .unwrap();
        assert_eq!(
            store.get(BlobKind::Source, &key).await.unwrap(),
            b"original"
        );
        assert!(store.exists(BlobKind::Source, &key).await.unwrap());
    }

    #[tokio::test]
    async fn missing_ancestors_of_the_root_are_created() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("not").join("yet").join("there");
        let store = BlobStore::new(&root);
        let key = store.put_source(b"x".to_vec(), b"").await.unwrap();
        assert_eq!(store.get(BlobKind::Source, &key).await.unwrap(), b"x");
        // A second blob in a new shard, and the same blob again, both succeed.
        let other = store.put_source(b"y".to_vec(), b"").await.unwrap();
        assert_eq!(store.get(BlobKind::Source, &other).await.unwrap(), b"y");
        store.put_source(b"x".to_vec(), b"").await.unwrap();
    }

    #[test]
    fn durable_creation_tolerates_existing_and_racing_directories() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("a").join("b");
        create_dir_durable(&target).unwrap();
        create_dir_durable(&target).unwrap();
        assert!(target.is_dir());
        // Concurrent creators of the same tree all succeed.
        let deep = dir.path().join("c").join("d").join("e");
        let handles: Vec<_> = (0..8)
            .map(|_| {
                let deep = deep.clone();
                std::thread::spawn(move || create_dir_durable(&deep))
            })
            .collect();
        for h in handles {
            h.join().unwrap().unwrap();
        }
        assert!(deep.is_dir());
    }

    #[test]
    fn keys_depend_on_every_input_unambiguously() {
        let k = |bytes: &[u8], salt: &[u8]| source_key(bytes, salt);
        assert_eq!(k(b"abc", b""), k(b"abc", b""));
        assert_ne!(k(b"abc", b""), k(b"abd", b""));
        assert_ne!(k(b"abc", b""), k(b"abc", b"user-salt"));
        // Length prefixes: moving bytes between fields changes the key.
        assert_ne!(k(b"ab", b"c"), k(b"a", b"bc"));
        assert_ne!(k(b"abc", b""), k(b"", b"abc"));
    }
}
