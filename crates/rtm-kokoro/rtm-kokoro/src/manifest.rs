// SPDX-License-Identifier: AGPL-3.0-or-later
//! The pinned model files (`models.sha256`), and checking a directory against them.

use std::io::Read;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

/// The manifest, as checked in.
pub const MANIFEST: &str = include_str!("../models.sha256");

/// The Hugging Face repository and revision the files come from.
pub const REPOSITORY: &str = "onnx-community/Kokoro-82M-v1.0-ONNX";
pub const REVISION: &str = "1939ad2a8e416c0acfeecc08a694d14ef25f2231";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelFile {
    pub sha256: String,
    /// Path in the Hugging Face repository.
    pub remote: String,
    /// Path under the local model directory.
    pub local: String,
}

impl ModelFile {
    /// Where to download it from, at the pinned revision.
    pub fn url(&self) -> String {
        format!(
            "https://huggingface.co/{REPOSITORY}/resolve/{REVISION}/{}",
            self.remote
        )
    }
}

/// Every file in the manifest.
pub fn files() -> Vec<ModelFile> {
    MANIFEST
        .lines()
        .filter(|l| !l.trim().is_empty() && !l.starts_with('#'))
        .map(|l| {
            let mut fields = l.split_whitespace();
            let mut next = || {
                fields
                    .next()
                    .expect("manifest lines have three fields")
                    .to_string()
            };
            ModelFile {
                sha256: next(),
                remote: next(),
                local: next(),
            }
        })
        .collect()
}

/// The voice ids the manifest provides, from `voices/<id>.bin`.
pub fn voice_ids() -> Vec<String> {
    files()
        .iter()
        .filter_map(|f| {
            f.local
                .strip_prefix("voices/")?
                .strip_suffix(".bin")
                .map(str::to_string)
        })
        .collect()
}

#[derive(Debug, thiserror::Error)]
pub enum VerifyError {
    #[error("{path} is missing; fetch the model files with `cargo xtask kokoro-models {dir}`")]
    Missing { path: PathBuf, dir: String },
    #[error("{path} doesn't match its pinned SHA-256 (expected {expected}, got {actual})")]
    Mismatch {
        path: PathBuf,
        expected: String,
        actual: String,
    },
    #[error("reading {path}: {source}")]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
}

/// SHA-256 of a file, as lowercase hex.
pub fn sha256_file(path: &Path) -> std::io::Result<String> {
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

/// Check every manifest file in `dir` against its pinned hash.
pub fn verify(dir: &Path) -> Result<(), VerifyError> {
    for file in files() {
        let path = dir.join(&file.local);
        if !path.is_file() {
            return Err(VerifyError::Missing {
                path,
                dir: dir.display().to_string(),
            });
        }
        let actual = sha256_file(&path).map_err(|source| VerifyError::Io {
            path: path.clone(),
            source,
        })?;
        if actual != file.sha256 {
            return Err(VerifyError::Mismatch {
                path,
                expected: file.sha256,
                actual,
            });
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_manifest_is_well_formed() {
        let files = files();
        assert!(
            files
                .iter()
                .any(|f| f.local == "model.onnx" && f.remote == "onnx/model.onnx")
        );
        assert!(files.iter().any(|f| f.local == "tokenizer.json"));
        for f in &files {
            assert_eq!(f.sha256.len(), 64, "{f:?}");
            assert!(
                f.sha256
                    .bytes()
                    .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
            );
            assert!(!f.local.contains(".."), "{f:?}");
        }
        assert!(voice_ids().contains(&"bf_emma".to_string()));
        assert!(files[0].url().contains(REVISION));
        // The revision constant matches the manifest comment the fetch tool reads.
        assert!(MANIFEST.contains(&format!("# Pinned to repository revision {REVISION}.")));
    }

    #[test]
    fn verification_catches_missing_and_altered_files() {
        let dir = std::env::temp_dir().join(format!("rtm-kokoro-verify-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        assert!(matches!(verify(&dir), Err(VerifyError::Missing { .. })));
        // Every file present but wrong.
        for f in files() {
            let path = dir.join(&f.local);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, b"not the model").unwrap();
        }
        assert!(matches!(verify(&dir), Err(VerifyError::Mismatch { .. })));
        std::fs::remove_dir_all(&dir).unwrap();
        assert_eq!(
            sha256_file(
                Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("models.sha256")
                    .as_path()
            )
            .unwrap()
            .len(),
            64
        );
    }
}
