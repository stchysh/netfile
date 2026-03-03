use netfile_core::{TransferService, ProgressTracker};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::fs;

#[tokio::test]
async fn test_transfer_service_creation() {
    let data_dir = PathBuf::from("/tmp/netfile_test_transfer");
    let download_dir = PathBuf::from("/tmp/netfile_test_download");

    fs::create_dir_all(&data_dir).await.unwrap();
    fs::create_dir_all(&download_dir).await.unwrap();

    let service = TransferService::new(
        0,
        3,
        1048576,
        data_dir.clone(),
        download_dir.clone(),
    )
    .await;

    assert!(service.is_ok());
    let service = service.unwrap();
    assert!(service.local_port() > 0);

    fs::remove_dir_all(&data_dir).await.ok();
    fs::remove_dir_all(&download_dir).await.ok();
}

#[tokio::test]
async fn test_transfer_service_with_compression() {
    let data_dir = PathBuf::from("/tmp/netfile_test_transfer_comp");
    let download_dir = PathBuf::from("/tmp/netfile_test_download_comp");

    fs::create_dir_all(&data_dir).await.unwrap();
    fs::create_dir_all(&download_dir).await.unwrap();

    let service = TransferService::new_with_compression(
        0,
        3,
        1048576,
        data_dir.clone(),
        download_dir.clone(),
        true,
    )
    .await;

    assert!(service.is_ok());

    fs::remove_dir_all(&data_dir).await.ok();
    fs::remove_dir_all(&download_dir).await.ok();
}

#[tokio::test]
async fn test_progress_tracker() {
    let tracker = Arc::new(ProgressTracker::new());

    tracker
        .start_transfer(
            "test-file-1".to_string(),
            "test.txt".to_string(),
            1000,
            10,
        )
        .await;

    let progress = tracker.get_progress("test-file-1").await;
    assert!(progress.is_some());

    let progress = progress.unwrap();
    assert_eq!(progress.file_id, "test-file-1");
    assert_eq!(progress.file_name, "test.txt");
    assert_eq!(progress.total_size, 1000);
    assert_eq!(progress.total_chunks, 10);

    tracker.update_progress("test-file-1", 100).await;
    let progress = tracker.get_progress("test-file-1").await.unwrap();
    assert_eq!(progress.transferred, 100);

    tracker.remove_progress("test-file-1").await;
    let progress = tracker.get_progress("test-file-1").await;
    assert!(progress.is_none());
}

#[test]
fn test_progress_calculations() {
    use netfile_core::TransferProgress;
    use std::time::Instant;

    let now = Instant::now();
    let progress = TransferProgress {
        file_id: "test".to_string(),
        file_name: "test.txt".to_string(),
        total_size: 1000,
        transferred: 500,
        total_chunks: 10,
        completed_chunks: 5,
        start_time: now,
        last_update: now,
    };

    assert_eq!(progress.progress_percent(), 50.0);
}
