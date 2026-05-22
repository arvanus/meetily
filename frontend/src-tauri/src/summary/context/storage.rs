//! Filesystem operations for context attachments.

use std::path::{Path, PathBuf};
use uuid::Uuid;

/// Maximum bytes read from a source file (anything beyond is truncated).
pub const MAX_ATTACHMENT_BYTES: usize = 256 * 1024;

#[derive(Debug)]
pub struct ReadResult {
    pub content: String,
    pub truncated: bool,
    pub size_bytes: usize,
}

#[derive(Debug, thiserror::Error)]
pub enum StorageError {
    #[error("file not found or unreadable: {0}")]
    NotReadable(String),
    #[error("file is not valid UTF-8 text")]
    NotText,
    #[error("meeting folder is not initialized")]
    MeetingFolderMissing,
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

/// Replace control chars and path separators in a filename; cap at 64 chars.
pub fn sanitize_basename(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| {
            if c == '/' || c == '\\' || c.is_control() {
                '_'
            } else {
                c
            }
        })
        .collect();
    // Strip any leading dots to avoid hidden files / "..".
    let trimmed = cleaned.trim_start_matches('.');
    let mut out = trimmed.to_string();
    if out.chars().count() > 64 {
        let end = out
            .char_indices()
            .nth(64)
            .map(|(i, _)| i)
            .unwrap_or(out.len());
        out.truncate(end);
    }
    if out.is_empty() {
        "attachment".to_string()
    } else {
        out
    }
}

/// Build the stored filename `<short-uuid>_<sanitized basename>`.
pub fn build_stored_filename(original_name: &str) -> String {
    let short_uuid = Uuid::new_v4()
        .to_string()
        .split('-')
        .next()
        .unwrap_or("att")
        .to_string();
    format!("{}_{}", short_uuid, sanitize_basename(original_name))
}

/// Read up to MAX_ATTACHMENT_BYTES from `src`. Reject if not valid UTF-8 text.
/// If file exceeds limit, truncate at the last newline within the window
/// (or at the window edge if no newline exists).
pub fn read_text_capped(src: &Path) -> Result<ReadResult, StorageError> {
    let bytes = std::fs::read(src)
        .map_err(|e| StorageError::NotReadable(format!("{}: {}", src.display(), e)))?;

    // Reject if it contains NUL bytes (a reliable binary indicator).
    if bytes.iter().any(|b| *b == 0) {
        return Err(StorageError::NotText);
    }

    let mut text = match std::str::from_utf8(&bytes) {
        Ok(s) => s.to_string(),
        Err(_) => return Err(StorageError::NotText),
    };

    let original_len = bytes.len();
    if original_len <= MAX_ATTACHMENT_BYTES {
        return Ok(ReadResult {
            content: text,
            truncated: false,
            size_bytes: original_len,
        });
    }

    // Truncate to MAX bytes by character boundary.
    let mut cut = MAX_ATTACHMENT_BYTES;
    while !text.is_char_boundary(cut) && cut > 0 {
        cut -= 1;
    }
    text.truncate(cut);

    // Prefer the last newline so we don't cut a line in half.
    if let Some(nl) = text.rfind('\n') {
        text.truncate(nl + 1);
    }

    Ok(ReadResult {
        content: text,
        truncated: true,
        size_bytes: original_len,
    })
}

/// Resolve `<meeting_folder>/attachments/` and create it if missing.
pub fn attachments_dir(meeting_folder: &Path) -> Result<PathBuf, StorageError> {
    if !meeting_folder.exists() {
        return Err(StorageError::MeetingFolderMissing);
    }
    let dir = meeting_folder.join("attachments");
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// Write `content` into `<meeting_folder>/attachments/<stored_filename>`.
pub fn write_attachment(
    meeting_folder: &Path,
    stored_filename: &str,
    content: &str,
) -> Result<PathBuf, StorageError> {
    let dir = attachments_dir(meeting_folder)?;
    let dest = dir.join(stored_filename);
    std::fs::write(&dest, content)?;
    Ok(dest)
}

/// Delete a stored attachment file. Missing file is not an error.
pub fn delete_attachment(meeting_folder: &Path, stored_filename: &str) -> Result<(), StorageError> {
    let dest = meeting_folder.join("attachments").join(stored_filename);
    if dest.exists() {
        std::fs::remove_file(&dest)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::TempDir;

    #[test]
    fn sanitize_replaces_path_separators() {
        assert_eq!(sanitize_basename("../etc/passwd"), "etc_passwd");
        assert_eq!(sanitize_basename("a\\b.txt"), "a_b.txt");
    }

    #[test]
    fn sanitize_replaces_control_chars() {
        assert_eq!(sanitize_basename("a\nb\tc.txt"), "a_b_c.txt");
    }

    #[test]
    fn sanitize_caps_at_64_chars() {
        let long = "a".repeat(100);
        let out = sanitize_basename(&long);
        assert_eq!(out.chars().count(), 64);
    }

    #[test]
    fn sanitize_handles_empty() {
        assert_eq!(sanitize_basename(""), "attachment");
        assert_eq!(sanitize_basename("..."), "attachment");
    }

    #[test]
    fn build_stored_filename_prefixes_uuid() {
        let name = build_stored_filename("notes.md");
        assert!(name.ends_with("_notes.md"));
        let parts: Vec<&str> = name.splitn(2, '_').collect();
        assert_eq!(parts.len(), 2);
        assert_eq!(parts[1], "notes.md");
    }

    fn write_temp(dir: &TempDir, name: &str, content: &[u8]) -> PathBuf {
        let p = dir.path().join(name);
        let mut f = std::fs::File::create(&p).unwrap();
        f.write_all(content).unwrap();
        p
    }

    #[test]
    fn read_text_capped_reads_small_file() {
        let dir = TempDir::new().unwrap();
        let p = write_temp(&dir, "small.md", b"hello\n");
        let r = read_text_capped(&p).unwrap();
        assert_eq!(r.content, "hello\n");
        assert!(!r.truncated);
        assert_eq!(r.size_bytes, 6);
    }

    #[test]
    fn read_text_capped_rejects_binary() {
        let dir = TempDir::new().unwrap();
        let p = write_temp(&dir, "bin", &[0x00, 0x01, 0x02]);
        let err = read_text_capped(&p).unwrap_err();
        assert!(matches!(err, StorageError::NotText));
    }

    #[test]
    fn read_text_capped_rejects_invalid_utf8() {
        let dir = TempDir::new().unwrap();
        // 0xC0 0x28 is invalid UTF-8 (and no NUL byte).
        let p = write_temp(&dir, "bad", &[0xC0, 0x28]);
        let err = read_text_capped(&p).unwrap_err();
        assert!(matches!(err, StorageError::NotText));
    }

    #[test]
    fn read_text_capped_truncates_oversized_at_newline() {
        let dir = TempDir::new().unwrap();
        let mut big = String::new();
        for _ in 0..30000 {
            big.push_str("line of about ten chars\n"); // 24 bytes each
        }
        assert!(big.len() > MAX_ATTACHMENT_BYTES);
        let p = write_temp(&dir, "big.txt", big.as_bytes());
        let r = read_text_capped(&p).unwrap();
        assert!(r.truncated);
        assert!(r.content.ends_with('\n'));
        assert!(r.content.len() <= MAX_ATTACHMENT_BYTES);
    }

    #[test]
    fn write_attachment_creates_directory_and_file() {
        let dir = TempDir::new().unwrap();
        let dest = write_attachment(dir.path(), "abc_notes.md", "hello").unwrap();
        assert!(dest.exists());
        assert_eq!(std::fs::read_to_string(&dest).unwrap(), "hello");
        assert!(dir.path().join("attachments").is_dir());
    }

    #[test]
    fn write_attachment_errors_when_meeting_folder_missing() {
        let nonexistent = PathBuf::from(
            "Z:/nonexistent/path/that/does/not/exist/anywhere/at/all",
        );
        let res = write_attachment(&nonexistent, "x.txt", "hi");
        assert!(matches!(
            res.unwrap_err(),
            StorageError::MeetingFolderMissing
        ));
    }

    #[test]
    fn delete_attachment_succeeds_when_file_missing() {
        let dir = TempDir::new().unwrap();
        std::fs::create_dir_all(dir.path().join("attachments")).unwrap();
        delete_attachment(dir.path(), "ghost.txt").unwrap();
    }

    #[test]
    fn delete_attachment_removes_file() {
        let dir = TempDir::new().unwrap();
        write_attachment(dir.path(), "x.txt", "hi").unwrap();
        delete_attachment(dir.path(), "x.txt").unwrap();
        assert!(!dir.path().join("attachments").join("x.txt").exists());
    }
}
