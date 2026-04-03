use futures_util::StreamExt;
use std::io::Write;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{mpsc, Semaphore};

/// Chunk size: 32MB — big enough for throughput, small enough to retry fast
const CHUNK_SIZE: u64 = 32 * 1024 * 1024;
/// How often to print progress
const PROGRESS_INTERVAL: Duration = Duration::from_secs(10);
/// Max retries per chunk before giving up
const MAX_RETRIES: u32 = 200;
/// Base retry delay (doubles on consecutive failures, capped at 30s)
const BASE_RETRY_DELAY: Duration = Duration::from_secs(2);
const MAX_RETRY_DELAY: Duration = Duration::from_secs(30);
/// HTTP timeouts
const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);
const CHUNK_TIMEOUT: Duration = Duration::from_secs(120);

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        eprintln!("Usage: stream-download <URL> [URL2 ...] [--threads N]");
        eprintln!("");
        eprintln!("Downloads one or more URLs sequentially to stdout.");
        eprintln!("Each URL is split into 32MB chunks downloaded by N parallel threads.");
        eprintln!("Failed chunks are retried individually — no full restarts.");
        eprintln!("Forces HTTP/1.1 to avoid HTTP/2 stream errors.");
        eprintln!("");
        eprintln!("Example: stream-download URL1 URL2 URL3 --threads 10 | tar -xf - -C /data");
        std::process::exit(1);
    }

    let num_threads: usize = args
        .iter()
        .position(|a| a == "--threads")
        .and_then(|i| args.get(i + 1))
        .and_then(|s| s.parse().ok())
        .unwrap_or(10);

    // Collect URLs (skip --threads and its value)
    let mut urls: Vec<String> = Vec::new();
    let mut skip_next = false;
    for arg in args.iter().skip(1) {
        if skip_next {
            skip_next = false;
            continue;
        }
        if arg == "--threads" {
            skip_next = true;
            continue;
        }
        urls.push(arg.clone());
    }

    let client = Arc::new(
        reqwest::Client::builder()
            .connect_timeout(CONNECT_TIMEOUT)
            .timeout(CHUNK_TIMEOUT)
            .tcp_keepalive(Duration::from_secs(15))
            .pool_max_idle_per_host(num_threads + 4)
            .http1_only() // Force HTTP/1.1
            .build()
            .expect("failed to build HTTP client"),
    );

    // Get sizes for all parts
    let mut part_sizes: Vec<u64> = Vec::new();
    for url in &urls {
        let size = get_content_length(&client, url).await;
        part_sizes.push(size);
    }
    let global_total: u64 = part_sizes.iter().sum();

    eprintln!(
        "=== stream-download v2 ===\n  Parts: {} | Total: {:.1} GB | Threads: {} | Chunk: {}MB | HTTP/1.1",
        urls.len(),
        global_total as f64 / 1e9,
        num_threads,
        CHUNK_SIZE / 1024 / 1024,
    );
    for (i, (url, size)) in urls.iter().zip(part_sizes.iter()).enumerate() {
        eprintln!(
            "  Part {}: {:.1} GB  ({})",
            i,
            *size as f64 / 1e9,
            url.split('/').last().unwrap_or(url),
        );
    }

    let global_start = Instant::now();
    let mut global_bytes: u64 = 0;
    let mut stdout = std::io::stdout().lock();

    for (part_idx, (url, &total_size)) in urls.iter().zip(part_sizes.iter()).enumerate() {
        let total_chunks = (total_size + CHUNK_SIZE - 1) / CHUNK_SIZE;

        eprintln!(
            "\n>>> Part {}/{}: {:.1} GB, {} chunks",
            part_idx + 1,
            urls.len(),
            total_size as f64 / 1e9,
            total_chunks,
        );

        let part_start = Instant::now();
        let semaphore = Arc::new(Semaphore::new(num_threads));

        // Buffer up to num_threads * 3 completed chunks before blocking producers
        let (tx, mut rx) = mpsc::channel::<(u64, Vec<u8>)>(num_threads * 3);

        let prod_client = client.clone();
        let prod_url = url.clone();
        let prod_sem = semaphore.clone();

        // Producer: launch all chunk downloads
        tokio::spawn(async move {
            for chunk_idx in 0..total_chunks {
                let permit = prod_sem.clone().acquire_owned().await.unwrap();
                let cl = prod_client.clone();
                let u = prod_url.clone();
                let tx = tx.clone();

                tokio::spawn(async move {
                    let start = chunk_idx * CHUNK_SIZE;
                    let end = std::cmp::min(start + CHUNK_SIZE - 1, total_size - 1);
                    let data = download_chunk(&cl, &u, start, end).await;
                    let _ = tx.send((chunk_idx, data)).await;
                    drop(permit);
                });
            }
            // tx drops here, closing the channel after all producers finish
        });

        // Consumer: write chunks to stdout in strict order
        let mut next_chunk: u64 = 0;
        let mut buffer: std::collections::BTreeMap<u64, Vec<u8>> = std::collections::BTreeMap::new();
        let mut part_bytes: u64 = 0;
        let mut last_progress = Instant::now();
        let mut retry_count: u64 = 0;

        while next_chunk < total_chunks {
            match rx.recv().await {
                Some((idx, data)) => {
                    buffer.insert(idx, data);

                    // Flush all consecutive ready chunks
                    while let Some(chunk_data) = buffer.remove(&next_chunk) {
                        if let Err(e) = stdout.write_all(&chunk_data) {
                            eprintln!("FATAL: stdout write error: {e}");
                            std::process::exit(1);
                        }
                        part_bytes += chunk_data.len() as u64;
                        global_bytes += chunk_data.len() as u64;
                        next_chunk += 1;
                    }

                    // Progress report
                    if last_progress.elapsed() >= PROGRESS_INTERVAL {
                        let elapsed = part_start.elapsed().as_secs_f64();
                        let speed_mbps = part_bytes as f64 / elapsed / 1e6;
                        let part_pct = part_bytes as f64 / total_size as f64 * 100.0;
                        let global_pct = global_bytes as f64 / global_total as f64 * 100.0;
                        let global_speed = global_bytes as f64 / global_start.elapsed().as_secs_f64() / 1e6;
                        let eta_min = (global_total - global_bytes) as f64 / (global_speed * 1e6) / 60.0;

                        eprintln!(
                            "  part{} {:.1}% ({:.1}/{:.1}GB) {:.0}MB/s | TOTAL {:.1}% ({:.0}/{:.0}GB) ETA {:.0}m | buf={} written={}",
                            part_idx,
                            part_pct,
                            part_bytes as f64 / 1e9,
                            total_size as f64 / 1e9,
                            speed_mbps,
                            global_pct,
                            global_bytes as f64 / 1e9,
                            global_total as f64 / 1e9,
                            eta_min,
                            buffer.len(),
                            next_chunk,
                        );
                        last_progress = Instant::now();
                    }
                }
                None => break,
            }
        }

        stdout.flush().ok();
        let elapsed = part_start.elapsed().as_secs_f64();
        eprintln!(
            "  part{} DONE: {:.1}GB in {:.0}min ({:.0}MB/s)",
            part_idx,
            part_bytes as f64 / 1e9,
            elapsed / 60.0,
            part_bytes as f64 / elapsed / 1e6,
        );
    }

    stdout.flush().ok();
    let elapsed = global_start.elapsed().as_secs_f64();
    eprintln!(
        "\n=== COMPLETE: {:.1}GB in {:.0}min ({:.0}MB/s avg) ===",
        global_bytes as f64 / 1e9,
        elapsed / 60.0,
        global_bytes as f64 / elapsed / 1e6,
    );
}

async fn get_content_length(client: &reqwest::Client, url: &str) -> u64 {
    for attempt in 0..20u32 {
        match client.head(url).send().await {
            Ok(resp) => {
                if let Some(len) = resp
                    .headers()
                    .get("content-length")
                    .and_then(|v| v.to_str().ok())
                    .and_then(|v| v.parse().ok())
                {
                    return len;
                }
            }
            Err(e) => {
                eprintln!("HEAD {} attempt {} failed: {}", url, attempt, e);
            }
        }
        tokio::time::sleep(Duration::from_secs(3)).await;
    }
    eprintln!("FATAL: could not get content-length for {url}");
    std::process::exit(1);
}

async fn download_chunk(client: &reqwest::Client, url: &str, start: u64, end: u64) -> Vec<u8> {
    let expected_size = (end - start + 1) as usize;
    let mut consecutive_failures: u32 = 0;

    for attempt in 0..MAX_RETRIES {
        // Build request with range header
        let resp = match client
            .get(url)
            .header("Range", format!("bytes={start}-{end}"))
            .send()
            .await
        {
            Ok(r) => r,
            Err(e) => {
                consecutive_failures += 1;
                let delay = backoff_delay(consecutive_failures);
                if attempt % 10 == 0 {
                    eprintln!(
                        "  retry chunk @{:.0}MB (attempt {}, wait {:.0}s): {}",
                        start as f64 / 1e6,
                        attempt,
                        delay.as_secs_f64(),
                        e,
                    );
                }
                tokio::time::sleep(delay).await;
                continue;
            }
        };

        let status = resp.status().as_u16();
        if status != 206 && status != 200 {
            consecutive_failures += 1;
            let delay = backoff_delay(consecutive_failures);
            if attempt % 10 == 0 {
                eprintln!(
                    "  retry chunk @{:.0}MB HTTP {} (attempt {}, wait {:.0}s)",
                    start as f64 / 1e6,
                    status,
                    attempt,
                    delay.as_secs_f64(),
                );
            }
            tokio::time::sleep(delay).await;
            continue;
        }

        // Stream body in pieces to detect mid-transfer failures
        let mut buf = Vec::with_capacity(expected_size);
        let mut stream = resp.bytes_stream();
        let mut stream_ok = true;

        while let Some(item) = stream.next().await {
            match item {
                Ok(chunk) => buf.extend_from_slice(&chunk),
                Err(e) => {
                    if attempt % 10 == 0 {
                        eprintln!(
                            "  retry chunk @{:.0}MB stream err at {}B (attempt {}): {}",
                            start as f64 / 1e6,
                            buf.len(),
                            attempt,
                            e,
                        );
                    }
                    stream_ok = false;
                    break;
                }
            }
        }

        if stream_ok && buf.len() == expected_size {
            return buf;
        }

        consecutive_failures += 1;
        let delay = backoff_delay(consecutive_failures);
        tokio::time::sleep(delay).await;
    }

    eprintln!("FATAL: chunk {start}-{end} failed after {MAX_RETRIES} retries");
    std::process::exit(1);
}

/// Exponential backoff: 2s, 4s, 8s, 16s, 30s (capped)
fn backoff_delay(consecutive: u32) -> Duration {
    let secs = BASE_RETRY_DELAY.as_secs() * 2u64.pow(consecutive.min(4));
    Duration::from_secs(secs.min(MAX_RETRY_DELAY.as_secs()))
}
