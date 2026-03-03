use anyhow::Result;
use std::path::{Path, PathBuf};
use tokio::fs;

#[derive(Debug, Clone)]
pub struct FileEntry {
    pub relative_path: PathBuf,
    pub size: u64,
    pub is_dir: bool,
}

pub async fn scan_directory(dir: &Path) -> Result<Vec<FileEntry>> {
    let mut entries = Vec::new();
    scan_recursive(dir, dir, &mut entries).await?;
    Ok(entries)
}

fn scan_recursive<'a>(
    base_dir: &'a Path,
    current_dir: &'a Path,
    entries: &'a mut Vec<FileEntry>,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<()>> + Send + 'a>> {
    Box::pin(async move {
        let mut read_dir = fs::read_dir(current_dir).await?;

        while let Some(entry) = read_dir.next_entry().await? {
            let path = entry.path();
            let metadata = entry.metadata().await?;

            let relative_path = path.strip_prefix(base_dir)?.to_path_buf();

            if metadata.is_dir() {
                entries.push(FileEntry {
                    relative_path: relative_path.clone(),
                    size: 0,
                    is_dir: true,
                });
                scan_recursive(base_dir, &path, entries).await?;
            } else {
                entries.push(FileEntry {
                    relative_path,
                    size: metadata.len(),
                    is_dir: false,
                });
            }
        }

        Ok(())
    })
}

pub fn calculate_total_size(entries: &[FileEntry]) -> u64 {
    entries.iter().filter(|e| !e.is_dir).map(|e| e.size).sum()
}

pub fn count_files(entries: &[FileEntry]) -> usize {
    entries.iter().filter(|e| !e.is_dir).count()
}
