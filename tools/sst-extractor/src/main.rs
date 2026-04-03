use alloy_primitives::B256;
use sha3::{Digest, Keccak256};
use std::collections::HashMap;
use std::path::Path;
use std::process::Command;

/// Known pool addresses to extract state for
const POOLS: &[(&str, &str)] = &[
    ("C6962004f452bE9203591991D15f6b388e09E8D0", "WETH/USDC 0.05% UniV3"),
    ("C31E54c7a869B9FcBEcc14363CF510d1c41fa443", "WETH/USDC 0.3% UniV3"),
    ("2f5e87C9312fa29aed5c179E456625D79015299c", "WETH/USDT 0.05% UniV3"),
    ("80A9ae39310abf666A87C743d6ebBD0E8C42158E", "WETH/ARB 0.05% UniV3"),
    ("d845f7D4f4DeB9Ff3bCeCe5A4E2D2B3f74b22Dc4", "WETH/WBTC 0.05% UniV3"),
    ("7fCdC35463E3770c2fB992716Cd070B63540b947", "WETH/USDC 0.01% PancakeV3"),
    ("d9e2A1a61B6E61b275cEc326465d417e52C1b95c", "WETH/USDC 0.05% PancakeV3"),
    ("18D3284d9EFf64Fc97b64aB2b871738677AE3632", "WETH/USDC 0.05% SushiV3"),
    ("B1026b8e7276e7AC75410F1fcbbe21796e8f7526", "WETH/USDC CamelotV3"),
    ("7CcCBA38E2D959fe135e79AEBB57CCb27B128358", "WETH/USDT CamelotV3"),
    // Balancer vault
    ("BA12222222228d8Ba445958a75a0704d566BF2C8", "Balancer Vault"),
    // Aave V3 Pool
    ("794a61358D6845594F94dc1DB02A252b5b4814aD", "Aave V3 Pool"),
];

/// Geth state DB key prefixes
/// Account hash → RLP(nonce, balance, storageRoot, codeHash)
/// Code hash → bytecode
/// 'h' prefix = header, 'H' = hash, 'b' = body, 'r' = receipts

fn keccak(data: &[u8]) -> B256 {
    let mut hasher = Keccak256::new();
    hasher.update(data);
    B256::from_slice(&hasher.finalize())
}

fn address_hash(addr: &str) -> B256 {
    let addr_bytes = hex::decode(addr).expect("invalid hex address");
    keccak(&addr_bytes)
}

/// Scan a single SST file using sst_dump CLI and extract data for known addresses
fn scan_sst_file(
    path: &Path,
    target_prefixes: &HashMap<String, String>,  // hex prefix (first 8 chars of hash) → name
    results: &mut HashMap<String, Vec<(String, String)>>,
    delete_after: bool,
) -> usize {
    // Use sst_dump to scan all key-value pairs
    let output = match Command::new("sst_dump")
        .arg(format!("--file={}", path.display()))
        .arg("--command=scan")
        .arg("--output_hex")
        .output()
    {
        Ok(o) => o,
        Err(e) => {
            eprintln!("  Skip {}: sst_dump error: {}", path.file_name().unwrap().to_str().unwrap(), e);
            return 0;
        }
    };

    if !output.status.success() {
        return 0;
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut found = 0;

    for line in stdout.lines() {
        // sst_dump output format: 'key_hex' seq:N, type:T => 'value_hex'
        if !line.contains("=>") {
            continue;
        }

        let parts: Vec<&str> = line.splitn(2, "=>").collect();
        if parts.len() != 2 {
            continue;
        }

        // Extract hex key (between quotes)
        let key_part = parts[0];
        let key_hex = extract_hex(key_part);
        if key_hex.len() < 8 {
            continue;
        }

        let value_hex = extract_hex(parts[1]);

        // Check first 8 hex chars (4 bytes) as fast prefix filter
        // Then check full 64-char hash (32 bytes) for match
        let prefix = &key_hex[..std::cmp::min(8, key_hex.len())];
        if let Some(name) = target_prefixes.get(prefix) {
            results
                .entry(name.clone())
                .or_default()
                .push((key_hex.clone(), value_hex));
            found += 1;
        }
    }

    // Delete SST file after processing to free disk
    if delete_after {
        if let Err(e) = std::fs::remove_file(path) {
            eprintln!("  Failed to delete {}: {}", path.display(), e);
        }
    }

    found
}

fn extract_hex(s: &str) -> String {
    // Extract hex string between single quotes
    if let Some(start) = s.find('\'') {
        if let Some(end) = s[start + 1..].find('\'') {
            return s[start + 1..start + 1 + end].to_string();
        }
    }
    String::new()
}

/// Store extracted data into Redis for the rpc-cache to serve
async fn store_in_redis(results: &HashMap<String, Vec<(String, String)>>) {
    let redis_url = std::env::var("REDIS_URL")
        .unwrap_or_else(|_| "redis://127.0.0.1:6379/1".to_string());

    let client = match redis::Client::open(redis_url.as_str()) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Redis connection failed: {}", e);
            return;
        }
    };
    let mut conn = match client.get_multiplexed_tokio_connection().await {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Redis connect failed: {}", e);
            return;
        }
    };

    use redis::AsyncCommands;

    for (name, entries) in results {
        let key = format!("sst:{}", name);
        let encoded: Vec<String> = entries
            .iter()
            .map(|(k, v)| format!("{}:{}", k, v))
            .collect();
        let json = serde_json::to_string(&encoded).unwrap();
        let _: Result<(), _> = conn.set_ex(&key, &json, 86400 * 7).await;
        println!("  Redis: {} → {} entries", key, entries.len());
    }

    println!("Redis storage complete");
}

/// Extract pool metadata from storage slots
fn extract_pool_metadata(entries: &[(String, String)]) -> HashMap<String, String> {
    let mut meta = HashMap::new();

    for (key, value) in entries {
        if key.len() < 64 || value.is_empty() {
            continue;
        }
        // key is hex string: address_hash (64 chars) + slot_hash (64 chars) = 128 chars
        if key.len() >= 128 {
            let slot_hex = &key[64..72]; // first 4 bytes of slot hash
            meta.insert(format!("slot_{}", slot_hex), value.clone());
        }
    }

    meta
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt().with_env_filter("info").init();

    let sst_dir = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "/root/arb-node-data/arb1/nitro/l2chaindata".to_string());

    let watch_mode = std::env::args().any(|a| a == "--watch");
    let delete_after = std::env::args().any(|a| a == "--delete");

    println!("=== SST Extractor ===");
    println!("Directory: {}", sst_dir);
    println!("Mode: {}", if watch_mode { "WATCH (live)" } else { "ONE-SHOT" });
    println!("Delete after: {}", delete_after);
    println!();

    // Build target prefix map: first 8 hex chars of keccak(address) → name
    let mut target_prefixes: HashMap<String, String> = HashMap::new();
    for (addr, name) in POOLS {
        let hash = address_hash(addr);
        let prefix = hex::encode(&hash.0[..4]); // first 4 bytes = 8 hex chars
        println!("  {} → {}...", name, &hex::encode(hash.0)[..16]);
        target_prefixes.insert(prefix, name.to_string());
    }
    println!("Tracking {} contracts", target_prefixes.len());
    println!();

    let mut processed: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut all_results: HashMap<String, Vec<(String, String)>> = HashMap::new();
    let mut total_found = 0;

    loop {
        let sst_path = Path::new(&sst_dir);

        // Wait for directory to exist (download in progress)
        if !sst_path.exists() {
            if watch_mode {
                println!("Waiting for {} to appear...", sst_dir);
                tokio::time::sleep(std::time::Duration::from_secs(10)).await;
                continue;
            } else {
                eprintln!("Directory does not exist: {}", sst_dir);
                return;
            }
        }

        // Find new SST files
        let sst_files: Vec<_> = std::fs::read_dir(sst_path)
            .unwrap_or_else(|_| panic!("Failed to read {}", sst_dir))
            .filter_map(|e| e.ok())
            .filter(|e| {
                let name = e.file_name().to_string_lossy().to_string();
                name.ends_with(".sst") && !processed.contains(&name)
            })
            .collect();

        if sst_files.is_empty() {
            if watch_mode {
                // Check if download is still running
                tokio::time::sleep(std::time::Duration::from_secs(5)).await;

                // Store intermediate results every cycle
                if total_found > 0 {
                    store_in_redis(&all_results).await;
                }
                continue;
            } else {
                break;
            }
        }

        let new_count = sst_files.len();
        println!("Processing {} new SST files (total processed: {})...", new_count, processed.len());

        for entry in &sst_files {
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().to_string();

            let found = scan_sst_file(&path, &target_prefixes, &mut all_results, delete_after);
            total_found += found;
            processed.insert(name.clone());

            if found > 0 {
                println!(
                    "  {} — {} entries (total: {})",
                    name, found, total_found
                );
            }
        }

        println!(
            "Batch done: {} files processed, {} total entries found, {} contracts with data",
            processed.len(),
            total_found,
            all_results.len()
        );

        // Store in Redis after each batch
        if total_found > 0 {
            store_in_redis(&all_results).await;
        }

        if !watch_mode {
            break;
        }

        // Brief sleep before checking for more files
        tokio::time::sleep(std::time::Duration::from_secs(3)).await;
    }

    println!();
    println!("=== Final Results ===");
    println!("Total SST files processed: {}", processed.len());
    println!("Total entries found: {}", total_found);
    for (name, entries) in &all_results {
        println!("  {}: {} entries", name, entries.len());
        let meta = extract_pool_metadata(entries);
        for (slot, value) in &meta {
            if value.len() <= 128 {
                println!("    {} = {}", slot, value);
            }
        }
    }
}
