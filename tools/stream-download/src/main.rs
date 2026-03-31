use futures_util::StreamExt;
use std::io::Write;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{mpsc, Semaphore};

const CHUNK_SIZE: u64 = 8 * 1024 * 1024; // 8MB per chunk — smaller = less server errors
const PROGRESS_INTERVAL: Duration = Duration::from_secs(10);
const MAX_RETRIES: u32 = 50;
const RETRY_DELAY: Duration = Duration::from_secs(3);

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().collect();
    let url = args.get(1).unwrap_or_else(|| {
        eprintln!("Usage: stream-download <URL> [--threads N]");
        std::process::exit(1);
    }).clone();

    let num_threads: usize = args.iter()
        .position(|a| a == "--threads")
        .and_then(|i| args.get(i + 1))
        .and_then(|s| s.parse().ok())
        .unwrap_or(8);

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(120))
        .connect_timeout(Duration::from_secs(30))
        .tcp_keepalive(Duration::from_secs(15))
        .pool_max_idle_per_host(num_threads + 2)
        .build()
        .expect("failed to build HTTP client");

    // Get total size
    let head = client.head(&url).send().await.expect("HEAD failed");
    let total_size = head
        .headers()
        .get("content-length")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(0);

    let total_chunks = (total_size + CHUNK_SIZE - 1) / CHUNK_SIZE;

    eprintln!(
        "Total: {:.1} GB | Chunks: {} x {:.0}MB | Threads: {}",
        total_size as f64 / 1e9,
        total_chunks,
        CHUNK_SIZE as f64 / 1e6,
        num_threads,
    );

    let start_time = Instant::now();
    let client = Arc::new(client);
    let url = Arc::new(url);
    let semaphore = Arc::new(Semaphore::new(num_threads));

    // Channel for completed chunks: (chunk_index, data)
    let (tx, mut rx) = mpsc::channel::<(u64, Vec<u8>)>(num_threads * 2);

    // Spawn producer: launches chunk downloads with semaphore limiting concurrency
    let producer_client = client.clone();
    let producer_url = url.clone();
    let producer_sem = semaphore.clone();
    tokio::spawn(async move {
        for chunk_idx in 0..total_chunks {
            let permit = producer_sem.clone().acquire_owned().await.unwrap();
            let client = producer_client.clone();
            let url = producer_url.clone();
            let tx = tx.clone();

            tokio::spawn(async move {
                let start = chunk_idx * CHUNK_SIZE;
                let end = std::cmp::min(start + CHUNK_SIZE - 1, total_size - 1);

                let data = download_chunk(&client, &url, start, end).await;
                tx.send((chunk_idx, data)).await.ok();
                drop(permit);
            });
        }
    });

    // Consumer: collect chunks and write to stdout IN ORDER
    let mut stdout = std::io::stdout().lock();
    let mut next_chunk: u64 = 0;
    let mut buffer: std::collections::BTreeMap<u64, Vec<u8>> = std::collections::BTreeMap::new();
    let mut bytes_written: u64 = 0;
    let mut last_progress = Instant::now();

    while next_chunk < total_chunks {
        match rx.recv().await {
            Some((idx, data)) => {
                buffer.insert(idx, data);

                // Write all consecutive ready chunks
                while let Some(chunk_data) = buffer.remove(&next_chunk) {
                    if let Err(e) = stdout.write_all(&chunk_data) {
                        eprintln!("stdout error: {}", e);
                        std::process::exit(1);
                    }
                    bytes_written += chunk_data.len() as u64;
                    next_chunk += 1;

                    // Progress
                    if last_progress.elapsed() >= PROGRESS_INTERVAL {
                        let elapsed = start_time.elapsed().as_secs_f64();
                        let speed = bytes_written as f64 / elapsed / 1e6;
                        let pct = bytes_written as f64 / total_size as f64 * 100.0;
                        let eta = (total_size - bytes_written) as f64 / (speed * 1e6) / 60.0;
                        eprintln!(
                            "[{:.1}/{:.1} GB] {:.1}% | {:.0} MB/s | ETA {:.0}min | buf={} | threads={}",
                            bytes_written as f64 / 1e9,
                            total_size as f64 / 1e9,
                            pct,
                            speed,
                            eta,
                            buffer.len(),
                            num_threads,
                        );
                        last_progress = Instant::now();
                    }
                }
            }
            None => break,
        }
    }

    stdout.flush().ok();
    let elapsed = start_time.elapsed().as_secs_f64();
    let speed = bytes_written as f64 / elapsed / 1e6;
    eprintln!(
        "Download complete! {:.1} GB in {:.0}min ({:.0} MB/s avg)",
        bytes_written as f64 / 1e9,
        elapsed / 60.0,
        speed,
    );
}

async fn download_chunk(client: &reqwest::Client, url: &str, start: u64, end: u64) -> Vec<u8> {
    let size = (end - start + 1) as usize;

    for attempt in 0..MAX_RETRIES {
        let resp = match client
            .get(url)
            .header("Range", format!("bytes={}-{}", start, end))
            .send()
            .await
        {
            Ok(r) => r,
            Err(e) => {
                eprintln!("Chunk {}-{} attempt {} failed: {}", start, end, attempt, e);
                tokio::time::sleep(RETRY_DELAY).await;
                continue;
            }
        };

        let status = resp.status().as_u16();
        if status != 206 && status != 200 {
            eprintln!("Chunk {}-{} HTTP {}", start, end, status);
            tokio::time::sleep(RETRY_DELAY).await;
            continue;
        }

        match resp.bytes().await {
            Ok(bytes) => {
                if bytes.len() == size {
                    return bytes.to_vec();
                }
                // Partial read — retry
                eprintln!(
                    "Chunk {}-{} partial: got {} of {} bytes",
                    start, end, bytes.len(), size
                );
                tokio::time::sleep(RETRY_DELAY).await;
            }
            Err(e) => {
                eprintln!("Chunk {}-{} read error: {}", start, end, e);
                tokio::time::sleep(RETRY_DELAY).await;
            }
        }
    }

    eprintln!("FATAL: chunk {}-{} failed after {} retries", start, end, MAX_RETRIES);
    std::process::exit(1);
}
