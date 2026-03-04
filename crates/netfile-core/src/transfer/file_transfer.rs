use super::file_utils::{calculate_chunk_checksum, read_file_chunk};
use super::state::TransferState;
use crate::protocol::{ChunkData, TransferRequest};
use anyhow::Result;
use bytes::Bytes;
use sha2::{Digest, Sha256};
use std::path::PathBuf;
use tokio::fs::File;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tracing::{debug, info};
use uuid::Uuid;

pub struct FileSender {
    file_path: PathBuf,
    file_id: String,
    relative_path: Option<String>,
    chunk_size: u32,
    data_dir: PathBuf,
}

impl FileSender {
    pub fn new(file_path: PathBuf, chunk_size: u32, data_dir: PathBuf) -> Self {
        let file_id = Uuid::new_v4().to_string();
        Self {
            file_path,
            file_id,
            relative_path: None,
            chunk_size,
            data_dir,
        }
    }

    pub fn with_relative_path(mut self, relative_path: String) -> Self {
        self.relative_path = Some(relative_path);
        self
    }

    pub async fn prepare(&self) -> Result<TransferRequest> {
        let metadata = tokio::fs::metadata(&self.file_path).await?;
        let file_size = metadata.len();

        let file_name = self
            .file_path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("unknown")
            .to_string();

        info!(
            "Prepared file for transfer: {} ({} bytes)",
            file_name, file_size
        );

        Ok(TransferRequest {
            file_id: self.file_id.clone(),
            file_name,
            relative_path: self.relative_path.clone(),
            file_size,
            chunk_size: self.chunk_size,
            device_id: String::new(),
            password_hash: None,
        })
    }

    pub async fn read_chunk(&self, chunk_index: u32) -> Result<ChunkData> {
        let mut file = File::open(&self.file_path).await?;
        let offset = chunk_index as u64 * self.chunk_size as u64;
        let data = read_file_chunk(&mut file, offset, self.chunk_size as usize).await?;
        let checksum = calculate_chunk_checksum(&data);

        debug!(
            "Read chunk {} ({} bytes) from file {}",
            chunk_index,
            data.len(),
            self.file_path.display()
        );

        Ok(ChunkData {
            file_id: self.file_id.clone(),
            chunk_index,
            data: Bytes::from(data),
            checksum,
            compressed: false,
        })
    }

    pub fn file_id(&self) -> &str {
        &self.file_id
    }
}

pub struct FileReceiver {
    state: TransferState,
    temp_file: Option<File>,
    data_dir: PathBuf,
    resume_from_chunk: u32,
    hasher: Sha256,
}

impl FileReceiver {
    pub async fn new(
        request: TransferRequest,
        save_dir: PathBuf,
        data_dir: PathBuf,
    ) -> Result<Self> {
        let file_path = if let Some(relative_path) = &request.relative_path {
            save_dir.join(relative_path)
        } else {
            save_dir.join(&request.file_name)
        };

        let temp_file_path = data_dir
            .join("temp")
            .join(format!("{}.tmp", request.file_id));

        let state_path = TransferState::state_file_path(&data_dir, &request.file_id);

        if state_path.exists() && temp_file_path.exists() {
            if let Ok(existing_state) = TransferState::load(&state_path) {
                if !existing_state.is_complete()
                    && existing_state.file_size == request.file_size
                    && existing_state.chunk_size == request.chunk_size
                {
                    let resume_from = existing_state.next_expected_chunk();
                    let temp_file = tokio::fs::OpenOptions::new()
                        .write(true)
                        .open(&temp_file_path)
                        .await?;

                    let mut hasher = Sha256::new();
                    let skip_bytes = (resume_from as u64 * existing_state.chunk_size as u64)
                        .min(existing_state.file_size);
                    if skip_bytes > 0 {
                        let mut read_file = File::open(&temp_file_path).await?;
                        let mut buf = vec![0u8; 4 * 1024 * 1024];
                        let mut remaining = skip_bytes;
                        while remaining > 0 {
                            let to_read = remaining.min(buf.len() as u64) as usize;
                            let n = read_file.read(&mut buf[..to_read]).await?;
                            if n == 0 {
                                break;
                            }
                            hasher.update(&buf[..n]);
                            remaining -= n as u64;
                        }
                    }

                    info!(
                        "Resuming file transfer: {} from chunk {} ({} chunks done)",
                        request.file_name,
                        resume_from,
                        existing_state.completed_chunks.len()
                    );

                    return Ok(Self {
                        state: existing_state,
                        temp_file: Some(temp_file),
                        data_dir,
                        resume_from_chunk: resume_from,
                        hasher,
                    });
                }
            }
        }

        if let Some(parent) = temp_file_path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }

        if let Some(parent) = file_path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }

        let state = TransferState::new(
            request.file_id.clone(),
            file_path,
            request.file_size,
            request.chunk_size,
            temp_file_path.clone(),
        );

        let state_clone = state.clone();
        let state_path_clone = state_path.clone();
        tokio::task::spawn_blocking(move || {
            let _ = state_clone.save(&state_path_clone);
        });

        let temp_file = File::create(&temp_file_path).await?;
        temp_file.set_len(request.file_size).await?;

        info!(
            "Created receiver for file: {} ({} bytes, {} chunks)",
            request.file_name, request.file_size, state.total_chunks
        );

        Ok(Self {
            state,
            temp_file: Some(temp_file),
            data_dir,
            resume_from_chunk: 0,
            hasher: Sha256::new(),
        })
    }

    pub async fn write_chunk(&mut self, chunk: ChunkData) -> Result<()> {
        if chunk.chunk_index >= self.state.total_chunks {
            return Err(anyhow::anyhow!("Invalid chunk index"));
        }

        let expected_checksum = calculate_chunk_checksum(chunk.data.as_ref());
        if expected_checksum != chunk.checksum {
            return Err(anyhow::anyhow!("Chunk checksum mismatch"));
        }

        let offset = chunk.chunk_index as u64 * self.state.chunk_size as u64;
        if let Some(file) = &mut self.temp_file {
            use tokio::io::{AsyncSeekExt, AsyncWriteExt};
            file.seek(std::io::SeekFrom::Start(offset)).await?;
            file.write_all(chunk.data.as_ref()).await?;
        }

        self.hasher.update(chunk.data.as_ref());
        self.state.completed_chunks.insert(chunk.chunk_index);

        debug!(
            "Wrote chunk {} ({} bytes), progress: {:.2}%",
            chunk.chunk_index,
            chunk.data.len(),
            self.state.progress() * 100.0
        );

        Ok(())
    }

    pub async fn write_chunk_raw(&mut self, chunk_index: u32, data: &[u8]) -> Result<()> {
        if chunk_index >= self.state.total_chunks {
            return Err(anyhow::anyhow!("Invalid chunk index"));
        }

        let offset = chunk_index as u64 * self.state.chunk_size as u64;
        if let Some(file) = &mut self.temp_file {
            use tokio::io::{AsyncSeekExt, AsyncWriteExt};
            file.seek(std::io::SeekFrom::Start(offset)).await?;
            file.write_all(data).await?;
        }

        self.hasher.update(data);
        self.state.completed_chunks.insert(chunk_index);

        if self.state.completed_chunks.len() % 32 == 0 {
            let state_clone = self.state.clone();
            let data_dir = self.data_dir.clone();
            tokio::task::spawn_blocking(move || {
                let path = TransferState::state_file_path(&data_dir, &state_clone.file_id);
                let _ = state_clone.save(&path);
            });
        }

        debug!(
            "Wrote chunk {} ({} bytes), progress: {:.2}%",
            chunk_index,
            data.len(),
            self.state.progress() * 100.0
        );

        Ok(())
    }

    pub async fn finalize(&mut self, expected_hash: [u8; 32]) -> Result<()> {
        if !self.state.is_complete() {
            return Err(anyhow::anyhow!("Transfer not complete"));
        }

        if let Some(mut file) = self.temp_file.take() {
            file.flush().await?;
            drop(file);
        }

        let final_hash_result = self.hasher.clone().finalize();
        let mut final_hash = [0u8; 32];
        final_hash.copy_from_slice(&final_hash_result);

        if final_hash != expected_hash {
            let _ = tokio::fs::remove_file(&self.state.temp_file_path).await;
            let state_path = TransferState::state_file_path(&self.data_dir, &self.state.file_id);
            let _ = tokio::fs::remove_file(&state_path).await;
            return Err(anyhow::anyhow!("File hash mismatch"));
        }

        if let Some(parent) = self.state.file_path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }

        if let Err(_) = tokio::fs::rename(&self.state.temp_file_path, &self.state.file_path).await {
            tokio::fs::copy(&self.state.temp_file_path, &self.state.file_path).await?;
            tokio::fs::remove_file(&self.state.temp_file_path).await?;
        }

        let state_path = TransferState::state_file_path(&self.data_dir, &self.state.file_id);
        if state_path.exists() {
            tokio::fs::remove_file(state_path).await?;
        }

        info!(
            "Transfer complete: {} saved to {}",
            self.state.file_id,
            self.state.file_path.display()
        );

        Ok(())
    }

    pub fn progress(&self) -> f64 {
        self.state.progress()
    }

    pub fn is_complete(&self) -> bool {
        self.state.is_complete()
    }

    pub fn file_id(&self) -> &str {
        &self.state.file_id
    }

    pub fn resume_from_chunk(&self) -> u32 {
        self.resume_from_chunk
    }

    pub async fn cleanup(&mut self) {
        if let Some(file) = self.temp_file.take() { drop(file); }
        let _ = tokio::fs::remove_file(&self.state.temp_file_path).await;
        let state_path = TransferState::state_file_path(&self.data_dir, &self.state.file_id);
        let _ = tokio::fs::remove_file(&state_path).await;
    }
}
