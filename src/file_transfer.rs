use anyhow::{Context, Result, anyhow};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

pub fn sanitize_filename(input: &str) -> Result<String> {
    let normalized = input.replace('\\', "/");
    let name = normalized
        .rsplit('/')
        .find(|part| !part.is_empty() && *part != "." && *part != "..")
        .ok_or_else(|| anyhow!("empty filename"))?;

    if name.contains('\0') || name == "." || name == ".." {
        return Err(anyhow!("invalid filename"));
    }

    Ok(name.to_string())
}

pub fn save_received_file(receive_dir: &Path, filename: &str, bytes: &[u8]) -> Result<PathBuf> {
    fs::create_dir_all(receive_dir)
        .with_context(|| format!("failed to create receive dir {}", receive_dir.display()))?;
    let safe_name = sanitize_filename(filename)?;
    let mut path = receive_dir.join(&safe_name);

    if path.exists() {
        let original = Path::new(&safe_name);
        let stem = original
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("file");
        let ext = original.extension().and_then(|e| e.to_str());
        let mut available_path = None;
        for index in 1..1000 {
            let candidate_name = match ext {
                Some(ext) => format!("{stem} ({index}).{ext}"),
                None => format!("{stem} ({index})"),
            };
            let candidate = receive_dir.join(candidate_name);
            if !candidate.exists() {
                available_path = Some(candidate);
                break;
            }
        }
        path = available_path.ok_or_else(|| anyhow!("no available non-overwriting filename"))?;
    }

    fs::write(&path, bytes).with_context(|| format!("failed to write {}", path.display()))?;
    Ok(path)
}

pub fn cleanup_old_received_files(
    receive_dir: &Path,
    retention: Duration,
    now: SystemTime,
) -> Result<usize> {
    if !receive_dir.exists() {
        return Ok(0);
    }

    let mut removed = 0;
    for entry in fs::read_dir(receive_dir)
        .with_context(|| format!("failed to read receive dir {}", receive_dir.display()))?
    {
        let entry = entry?;
        let metadata = entry.metadata()?;
        if !metadata.is_file() {
            continue;
        }

        let Ok(modified) = metadata.modified() else {
            continue;
        };
        let Ok(age) = now.duration_since(modified) else {
            continue;
        };

        if age > retention {
            fs::remove_file(entry.path())
                .with_context(|| format!("failed to remove old file {}", entry.path().display()))?;
            removed += 1;
        }
    }

    Ok(removed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn sanitizes_path_to_basename() {
        assert_eq!(sanitize_filename("../secret.txt").unwrap(), "secret.txt");
        assert_eq!(
            sanitize_filename("C:\\temp\\report.pdf").unwrap(),
            "report.pdf"
        );
    }

    #[test]
    fn rejects_empty_filename() {
        assert!(sanitize_filename("../").is_err());
    }

    #[test]
    fn saves_without_overwriting() {
        let root = std::env::temp_dir().join(format!("rustclipsync-test-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("a.txt"), b"old").unwrap();
        let first = save_received_file(&root, "a.txt", b"new").unwrap();
        assert_eq!(first.file_name().unwrap().to_string_lossy(), "a (1).txt");
        assert_eq!(fs::read(first).unwrap(), b"new");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn errors_when_no_non_overwriting_name_is_available() {
        let root = std::env::temp_dir().join(format!("rustclipsync-test-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("a.txt"), b"old").unwrap();
        for index in 1..1000 {
            fs::write(root.join(format!("a ({index}).txt")), b"old").unwrap();
        }

        let result = save_received_file(&root, "a.txt", b"new");

        assert!(result.is_err());
        assert_eq!(fs::read(root.join("a.txt")).unwrap(), b"old");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn cleanup_removes_only_old_regular_files() {
        let root =
            std::env::temp_dir().join(format!("rustclipsync-cleanup-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&root).unwrap();
        let old_file = root.join("old.txt");
        let fresh_file = root.join("fresh.txt");
        let old_dir = root.join("old-dir");
        fs::write(&old_file, b"old").unwrap();
        fs::write(&fresh_file, b"fresh").unwrap();
        fs::create_dir_all(&old_dir).unwrap();

        let now = std::time::SystemTime::now();
        let old_time = now - std::time::Duration::from_secs(25 * 60 * 60);
        filetime::set_file_mtime(&old_file, filetime::FileTime::from_system_time(old_time))
            .unwrap();
        filetime::set_file_mtime(&old_dir, filetime::FileTime::from_system_time(old_time)).unwrap();

        cleanup_old_received_files(&root, std::time::Duration::from_secs(24 * 60 * 60), now)
            .unwrap();

        assert!(!old_file.exists());
        assert!(fresh_file.exists());
        assert!(old_dir.exists());
        fs::remove_dir_all(root).unwrap();
    }
}
