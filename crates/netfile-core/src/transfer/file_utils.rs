use sha2::{Digest, Sha256};
use std::path::Path;
use tokio::fs::File;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

pub async fn calculate_file_hash(path: &Path) -> anyhow::Result<[u8; 32]> {
    let mut file = File::open(path).await?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; 4 * 1024 * 1024];

    loop {
        let n = file.read(&mut buffer).await?;
        if n == 0 {
            break;
        }
        hasher.update(&buffer[..n]);
    }

    let result = hasher.finalize();
    let mut hash = [0u8; 32];
    hash.copy_from_slice(&result);
    Ok(hash)
}

pub fn calculate_chunk_checksum(data: &[u8]) -> u32 {
    let mut hasher = crc32fast::Hasher::new();
    hasher.update(data);
    hasher.finalize()
}

pub async fn read_file_chunk(
    file: &mut File,
    offset: u64,
    size: usize,
) -> anyhow::Result<Vec<u8>> {
    use tokio::io::AsyncSeekExt;

    file.seek(std::io::SeekFrom::Start(offset)).await?;
    let mut buffer = vec![0u8; size];
    let n = file.read(&mut buffer).await?;
    buffer.truncate(n);
    Ok(buffer)
}

pub async fn write_file_chunk(
    file: &mut File,
    offset: u64,
    data: &[u8],
) -> anyhow::Result<()> {
    use tokio::io::AsyncSeekExt;

    file.seek(std::io::SeekFrom::Start(offset)).await?;
    file.write_all(data).await?;
    Ok(())
}
