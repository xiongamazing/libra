//! Helpers to read or write compressed git objects on disk, returning raw payloads and computing their object hashes.

use std::{
    fs,
    io::{Read, Write},
    path::Path,
};

use flate2::read::ZlibDecoder;
use git_internal::{errors::GitError, hash::ObjectHash};
/// Helper function to read and decompress a git object from the object database.
pub fn read_git_object(git_dir: &Path, hash: &ObjectHash) -> Result<Vec<u8>, GitError> {
    let hash_str = hash.to_string();
    let object_path = git_dir
        .join("objects")
        .join(&hash_str[..2])
        .join(&hash_str[2..]);

    let file = fs::File::open(object_path)?;
    let mut decoder = ZlibDecoder::new(file);
    let mut buffer = Vec::new();
    decoder.read_to_end(&mut buffer)?;

    // The buffer now contains "<type> <size>\0<content>", where <type> is the git object type (e.g., commit, tree, blob, tag)
    // Strip the header (which contains the object type and size) to obtain only the object content.
    if let Some(header_end) = buffer.iter().position(|&b| b == 0) {
        Ok(buffer[header_end + 1..].to_vec())
    } else {
        Err(GitError::InvalidObjectInfo(
            "Could not find object header terminator".to_string(),
        ))
    }
}

/// Helper function to write a git object to the object database.
pub fn write_git_object(
    git_dir: &Path,
    object_type: &str,
    data: &[u8],
) -> Result<ObjectHash, GitError> {
    let header = format!("{} {}\0", object_type, data.len());
    let mut content = header.into_bytes();
    content.extend_from_slice(data);
    let hash = ObjectHash::new(&content);
    let hash_str = hash.to_string();

    let object_path = git_dir
        .join("objects")
        .join(&hash_str[..2])
        .join(&hash_str[2..]);

    if !object_path.exists() {
        // INVARIANT: `object_path` is built by joining `git_dir` with three
        // additional components ("objects", first-2-of-hash, rest-of-hash),
        // so `.parent()` always returns the directory holding the loose
        // object file.
        let parent = object_path
            .parent()
            .expect("loose-object path always has a parent directory");
        fs::create_dir_all(parent)?;
        let file = fs::File::create(object_path)?;
        let mut encoder = flate2::write::ZlibEncoder::new(file, flate2::Compression::default());
        encoder.write_all(&content)?;
        encoder.finish()?;
    }

    Ok(hash)
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::*;

    #[test]
    fn write_and_read_blob_roundtrip() {
        let dir = tempdir().expect("create tempdir");
        let data = b"hello, libra";
        let hash = write_git_object(dir.path(), "blob", data).expect("write blob");

        let content = read_git_object(dir.path(), &hash).expect("read blob");
        assert_eq!(content, data);
    }

    #[test]
    fn write_idempotent_same_content() {
        let dir = tempdir().expect("create tempdir");
        let data = b"same content";
        let h1 = write_git_object(dir.path(), "blob", data).expect("first write");
        let h2 = write_git_object(dir.path(), "blob", data).expect("second write");
        assert_eq!(h1, h2, "same content should produce the same hash");
    }

    #[test]
    fn write_different_types_produces_different_hashes() {
        let dir = tempdir().expect("create tempdir");
        let data = b"payload";
        let h_blob = write_git_object(dir.path(), "blob", data).expect("write blob");
        let h_commit = write_git_object(dir.path(), "commit", data).expect("write commit");
        assert_ne!(h_blob, h_commit, "different object types should differ");
    }

    #[test]
    fn read_nonexistent_object_returns_error() {
        let dir = tempdir().expect("create tempdir");
        let fake_hash = ObjectHash::new(b"nonexistent");
        assert!(read_git_object(dir.path(), &fake_hash).is_err());
    }

    #[test]
    fn write_empty_blob() {
        let dir = tempdir().expect("create tempdir");
        let hash = write_git_object(dir.path(), "blob", b"").expect("write empty blob");
        let content = read_git_object(dir.path(), &hash).expect("read empty blob");
        assert!(content.is_empty());
    }

    #[test]
    fn write_large_blob() {
        let dir = tempdir().expect("create tempdir");
        let data = vec![0xABu8; 64 * 1024]; // 64 KiB
        let hash = write_git_object(dir.path(), "blob", &data).expect("write large blob");
        let content = read_git_object(dir.path(), &hash).expect("read large blob");
        assert_eq!(content.len(), 64 * 1024);
        assert_eq!(content, data);
    }
}
