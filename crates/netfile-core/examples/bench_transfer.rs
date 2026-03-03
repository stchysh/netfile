use netfile_core::transfer::file_utils::calculate_file_hash;
use netfile_core::TransferService;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();

    if args.len() < 2 {
        eprintln!("Usage: bench_transfer <file_path> [chunk_mb]");
        eprintln!("  file_path  path to the file to transfer");
        eprintln!("  chunk_mb   chunk size in MB (default: 8)");
        std::process::exit(1);
    }

    let file_path = PathBuf::from(&args[1]);
    let chunk_mb: u32 = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(8);
    let chunk_size: u32 = chunk_mb * 1024 * 1024;

    if !file_path.exists() {
        eprintln!("File not found: {}", file_path.display());
        std::process::exit(1);
    }

    let metadata = tokio::fs::metadata(&file_path).await?;
    let file_size = metadata.len();

    println!("=== netfile bench_transfer ===");
    println!("File:       {}", file_path.display());
    println!("Size:       {:.3} GB ({} bytes)", file_size as f64 / 1_073_741_824.0, file_size);
    println!("Chunk size: {} MB", chunk_mb);
    println!();

    let base_dir = std::env::temp_dir().join("netfile_bench");
    let data_dir = base_dir.join("data");
    let download_dir = base_dir.join("download");
    tokio::fs::create_dir_all(&data_dir).await?;
    tokio::fs::create_dir_all(&download_dir).await?;

    println!("[1/4] Computing source SHA-256 (benchmark verification only, not part of transfer)...");
    let t0 = Instant::now();
    let source_hash = calculate_file_hash(&file_path).await?;
    let hash_elapsed = t0.elapsed();
    println!(
        "      {:02x}{:02x}{:02x}{:02x}... ({:.2}s, {:.0} MB/s)",
        source_hash[0], source_hash[1], source_hash[2], source_hash[3],
        hash_elapsed.as_secs_f64(),
        file_size as f64 / 1_048_576.0 / hash_elapsed.as_secs_f64()
    );
    println!();

    println!("[2/4] Starting receiver service...");
    let receiver = Arc::new(
        TransferService::new(37077, 1, chunk_size, data_dir.clone(), download_dir.clone()).await?,
    );
    let receiver_port = receiver.local_port();
    {
        let svc = receiver.clone();
        tokio::spawn(async move {
            svc.start().await;
        });
    }
    println!("      Listening on 127.0.0.1:{}", receiver_port);
    println!();

    println!("[3/4] Transferring (includes sender pre-hash)...");
    let sender = Arc::new(
        TransferService::new(0, 1, chunk_size, data_dir.clone(), download_dir.clone()).await?,
    );

    let tracker = sender.progress_tracker();
    let progress_task = tokio::spawn(async move {
        loop {
            tokio::time::sleep(tokio::time::Duration::from_millis(300)).await;
            let all = tracker.list_all().await;
            if let Some(p) = all.first() {
                use std::io::Write;
                print!("\r      {}", p.display());
                std::io::stdout().flush().ok();
            }
        }
    });

    let target: SocketAddr = format!("127.0.0.1:{}", receiver_port).parse()?;
    let t1 = Instant::now();
    sender.send_file(file_path.clone(), target).await?;
    let transfer_elapsed = t1.elapsed();
    progress_task.abort();

    let speed = file_size as f64 / 1_048_576.0 / transfer_elapsed.as_secs_f64();
    println!("\r      Transfer complete.                                    ");
    println!("      Time:  {:.2}s", transfer_elapsed.as_secs_f64());
    println!("      Speed: {:.2} MB/s  ({:.3} Gbps)", speed, speed * 8.0 / 1024.0);
    println!();

    println!("[4/4] Verifying received file SHA-256...");
    let file_name = file_path.file_name().unwrap();
    let received_path = download_dir.join(file_name);

    if !received_path.exists() {
        eprintln!("      ERROR: received file not found at {}", received_path.display());
        tokio::fs::remove_dir_all(&base_dir).await.ok();
        std::process::exit(1);
    }

    let received_meta = tokio::fs::metadata(&received_path).await?;
    if received_meta.len() != file_size {
        eprintln!(
            "      ERROR: size mismatch — expected {} bytes, got {}",
            file_size,
            received_meta.len()
        );
        tokio::fs::remove_dir_all(&base_dir).await.ok();
        std::process::exit(1);
    }

    let t2 = Instant::now();
    let received_hash = calculate_file_hash(&received_path).await?;
    let verify_elapsed = t2.elapsed();

    if source_hash == received_hash {
        println!("      PASS  ({:.2}s)", verify_elapsed.as_secs_f64());
    } else {
        eprintln!("      FAIL  hash mismatch!");
        eprintln!(
            "      expected {:02x}{:02x}{:02x}{:02x}...",
            source_hash[0], source_hash[1], source_hash[2], source_hash[3]
        );
        eprintln!(
            "      got      {:02x}{:02x}{:02x}{:02x}...",
            received_hash[0], received_hash[1], received_hash[2], received_hash[3]
        );
        tokio::fs::remove_dir_all(&base_dir).await.ok();
        std::process::exit(1);
    }

    tokio::fs::remove_dir_all(&base_dir).await.ok();

    println!();
    println!("=== Summary ===");
    println!("  File size     {:.3} GB", file_size as f64 / 1_073_741_824.0);
    println!("  Chunk size    {} MB", chunk_mb);
    println!("  Hash (bench)  {:.2}s  (benchmark verification, not protocol overhead)", hash_elapsed.as_secs_f64());
    println!("  Transfer      {:.2}s  ({:.2} MB/s)", transfer_elapsed.as_secs_f64(), speed);
    println!("  Verify        {:.2}s", verify_elapsed.as_secs_f64());

    Ok(())
}
