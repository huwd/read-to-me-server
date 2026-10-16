// SPDX-License-Identifier: AGPL-3.0-or-later
//! `cargo xtask kokoro-models <dir>`: download the Kokoro model files pinned in
//! `crates/rtm-kokoro/models.sha256` and verify each against its SHA-256 (ADR-0007,
//! ADR-0014). Files already present and correct are left alone.
//!
//! Downloads use `curl`: the workspace bans HTTP client crates outside the egress guard,
//! and this is a developer and image-build tool, not part of the server.

use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::process::Command;

use sha2::{Digest, Sha256};

/// The prefix of the manifest comment naming the pinned repository revision.
const REVISION_PREFIX: &str = "# Pinned to repository revision ";
const REPOSITORY: &str = "onnx-community/Kokoro-82M-v1.0-ONNX";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub sha256: String,
    pub remote: String,
    pub local: String,
}

/// The revision and the files in a manifest.
pub fn parse(manifest: &str) -> io::Result<(String, Vec<Entry>)> {
    let bad = |m: String| io::Error::new(io::ErrorKind::InvalidData, m);
    let revision = manifest
        .lines()
        .find_map(|l| l.strip_prefix(REVISION_PREFIX))
        .map(|r| r.trim_end_matches('.').trim().to_string())
        .ok_or_else(|| bad("the manifest names no revision".into()))?;
    let mut entries = Vec::new();
    for line in manifest
        .lines()
        .filter(|l| !l.trim().is_empty() && !l.starts_with('#'))
    {
        let fields: Vec<&str> = line.split_whitespace().collect();
        let [sha256, remote, local] = fields[..] else {
            return Err(bad(format!("bad manifest line: {line}")));
        };
        if local.contains("..") || local.starts_with('/') {
            return Err(bad(format!("unsafe path in manifest: {local}")));
        }
        entries.push(Entry {
            sha256: sha256.into(),
            remote: remote.into(),
            local: local.into(),
        });
    }
    Ok((revision, entries))
}

fn sha256(path: &Path) -> io::Result<String> {
    let mut file = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect())
}

/// Fetch every manifest file into `dir`. Returns the paths downloaded.
pub fn fetch(root: &Path, dir: &Path) -> io::Result<Vec<PathBuf>> {
    let manifest = std::fs::read_to_string(root.join("crates/rtm-kokoro/models.sha256"))?;
    let (revision, entries) = parse(&manifest)?;
    let mut fetched = Vec::new();
    for entry in entries {
        let path = dir.join(&entry.local);
        if path.is_file() && sha256(&path)? == entry.sha256 {
            println!("ok        {}", entry.local);
            continue;
        }
        std::fs::create_dir_all(path.parent().expect("files have a parent"))?;
        let partial = path.with_extension("partial");
        let url = format!(
            "https://huggingface.co/{REPOSITORY}/resolve/{revision}/{}",
            entry.remote
        );
        println!("fetching  {} from {url}", entry.local);
        let status = Command::new("curl")
            .args([
                "--fail",
                "--location",
                "--silent",
                "--show-error",
                "--retry",
                "3",
            ])
            .arg("--output")
            .arg(&partial)
            .arg(&url)
            .status()?;
        if !status.success() {
            let _ = std::fs::remove_file(&partial);
            return Err(io::Error::other(format!(
                "downloading {url} failed ({status})"
            )));
        }
        let actual = sha256(&partial)?;
        if actual != entry.sha256 {
            let _ = std::fs::remove_file(&partial);
            return Err(io::Error::other(format!(
                "{} doesn't match its pinned SHA-256 (expected {}, got {actual})",
                entry.local, entry.sha256
            )));
        }
        std::fs::rename(&partial, &path)?;
        fetched.push(path);
    }
    Ok(fetched)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_checked_in_manifest_parses() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
        let manifest =
            std::fs::read_to_string(root.join("crates/rtm-kokoro/models.sha256")).unwrap();
        let (revision, entries) = parse(&manifest).unwrap();
        assert_eq!(revision.len(), 40);
        assert!(entries.iter().any(|e| e.local == "model.onnx"));
    }

    #[test]
    fn unsafe_or_malformed_manifests_are_refused() {
        let head = "# Pinned to repository revision abc.\n";
        assert!(parse(&format!("{head}aa  x  ../escape")).is_err());
        assert!(parse(&format!("{head}aa  x  /etc/passwd")).is_err());
        assert!(parse(&format!("{head}aa  only-two")).is_err());
        assert!(parse("aa  x  y").is_err(), "no revision");
        assert_eq!(parse(&format!("{head}aa  x  y")).unwrap().0, "abc");
    }
}
