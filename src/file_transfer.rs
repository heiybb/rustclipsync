use anyhow::{Context, Result, anyhow};
use std::fs;
use std::path::{Path, PathBuf};

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
}
