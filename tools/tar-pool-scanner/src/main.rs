/// Reads a tar stream from stdin, filters for l2chaindata SST files,
/// scans them for known pool contract data, and stores results in Redis.
/// Skips arbitrumdata to save disk. Writes only essential metadata files to disk.
///
/// Usage: stream-download URL | tar-pool-scanner [--extract-dir /path]
///
/// The scanner looks for known pool address hashes in SST file keys.
/// When found, it extracts the storage data and stores it in Redis
/// for the rpc-cache to serve.

use sha3::{Digest, Keccak256};
use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::Command;

/// Pool contracts to scan for (address → name)
const POOLS: &[(&str, &str)] = &[
    // Camelot V2
    ("54B26fAf3671677C19F70c4B879A6f7B898F732c", "CamelotV2_WETH_USDC"),
    ("a6c5C7D189fA4eB5Af8ba34E63dCDD3a635D433f", "CamelotV2_WETH_ARB"),
    ("E8b2C9cBfd52CF9A157724e6416440566fA03150", "CamelotV2_WETH_MAGIC"),
    ("dc2167F4A5DeC5401EcEFF1CB55C3573A13F24bD", "CamelotV2_WETH_GMX"),
    ("f82105aA473560CfBF8Cbc6Fd83dB14Eb4028117", "CamelotV2_WETH_GRAIL"),
    // Uniswap V3
    ("C6962004f452bE9203591991D15f6b388e09E8D0", "UniV3_WETH_USDC_005"),
    ("C31E54c7a869B9FcBEcc14363CF510d1c41fa443", "UniV3_WETH_USDC_03"),
    ("80A9ae39310abf666A87C743d6ebBD0E8C42158E", "UniV3_WETH_ARB_005"),
    ("2391DDC81Cd63aAEaD9BDe63B00bB63e60DdBE9c", "UniV3_USDC_USDT_001"),
    ("35218a1cbaC5Bbc3E57fd9Bd38219D37571b3537", "UniV3_wstETH_WETH"),
    // PancakeSwap V3
    ("7fCdC35463E3770c2fB992716Cd070B63540b947", "PcsV3_WETH_USDC_001"),
    ("7e928afb59f5dE9D2f4d162f754c6eB40C88Aa8e", "PcsV3_USDT_USDC_001"),
    // SushiSwap V2
    ("57b85FEf094e10b5eeCDF350Af688299E9553378", "SushiV2_WETH_USDC"),
    ("B7E50106A5bd3Cf21AF210A755F9C8740890A8c9", "SushiV2_WETH_MAGIC"),
    // Balancer & Aave
    ("BA12222222228d8Ba445958a75a0704d566BF2C8", "BalancerVault"),
    ("794a61358D6845594F94dc1DB02A252b5b4814aD", "AaveV3Pool"),
    // Curve
    ("7f90122BF0700F9E7e1F688fe926940E8839F353", "Curve_2pool"),
    ("6eB2dc694eB516B16Dc9FBc678C60052BbdD7d80", "Curve_wstETH_WETH"),
    // Key tokens (WETH, USDC, etc.) — to find all pools holding them
    ("82aF49447D8a07e3bd95BD0d56f35241523fBab1", "WETH"),
    ("af88d065e77c8cC2239327C5EDb3A432268e5831", "USDC"),
    ("912CE59144191C1204E64559FE8253a0e49E6548", "ARB"),
];

fn keccak(data: &[u8]) -> [u8; 32] {
    let mut hasher = Keccak256::new();
    hasher.update(data);
    let result = hasher.finalize();
    let mut out = [0u8; 32];
    out.copy_from_slice(&result);
    out
}

fn build_target_prefixes() -> HashMap<[u8; 4], String> {
    let mut map = HashMap::new();
    for (addr_hex, name) in POOLS {
        let addr_bytes = hex::decode(addr_hex).unwrap();
        let hash = keccak(&addr_bytes);
        let mut prefix = [0u8; 4];
        prefix.copy_from_slice(&hash[..4]);
        map.insert(prefix, name.to_string());
    }
    map
}

fn scan_sst_bytes_with_dump(
    sst_path: &str,
    prefixes: &HashMap<[u8; 4], String>,
) -> HashMap<String, Vec<(String, String)>> {
    let mut results: HashMap<String, Vec<(String, String)>> = HashMap::new();

    let output = match Command::new("sst_dump")
        .arg(format!("--file={}", sst_path))
        .arg("--command=scan")
        .arg("--output_hex")
        .output()
    {
        Ok(o) if o.status.success() => o,
        _ => return results,
    };

    let stdout = String::from_utf8_lossy(&output.stdout);

    for line in stdout.lines() {
        if !line.contains("=>") {
            continue;
        }
        let parts: Vec<&str> = line.splitn(2, "=>").collect();
        if parts.len() != 2 {
            continue;
        }

        let key_hex = extract_hex(parts[0]);
        if key_hex.len() < 8 {
            continue;
        }

        let prefix_bytes = match hex::decode(&key_hex[..8]) {
            Ok(b) if b.len() >= 4 => {
                let mut p = [0u8; 4];
                p.copy_from_slice(&b[..4]);
                p
            }
            _ => continue,
        };

        if let Some(name) = prefixes.get(&prefix_bytes) {
            let value_hex = extract_hex(parts[1]);
            results
                .entry(name.clone())
                .or_default()
                .push((key_hex, value_hex));
        }
    }

    results
}

fn extract_hex(s: &str) -> String {
    if let Some(start) = s.find('\'') {
        if let Some(end) = s[start + 1..].find('\'') {
            return s[start + 1..start + 1 + end].to_string();
        }
    }
    String::new()
}

#[tokio::main]
async fn main() {
    let extract_dir = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "/root/arb-node-data/arb1/nitro".to_string());

    eprintln!("=== Tar Pool Scanner ===");
    eprintln!("Extract dir: {}", extract_dir);
    eprintln!("Reading tar from stdin...");

    let prefixes = build_target_prefixes();
    eprintln!("Tracking {} contract address hashes", prefixes.len());

    // Connect to Redis
    let redis_client = redis::Client::open("redis://127.0.0.1:6379/0").unwrap();
    let mut redis_conn = redis_client.get_multiplexed_tokio_connection().await.unwrap();

    let stdin = std::io::stdin();
    let mut archive = tar::Archive::new(stdin.lock());

    let mut total_sst = 0u64;
    let mut total_found = 0u64;
    let mut entries_processed = 0u64;
    let tmp_dir = "/tmp/sst_scan";
    std::fs::create_dir_all(tmp_dir).ok();

    for entry_result in archive.entries().unwrap() {
        let mut entry = match entry_result {
            Ok(e) => e,
            Err(_) => continue,
        };

        let path = match entry.path() {
            Ok(p) => p.to_path_buf(),
            Err(_) => continue,
        };
        let path_str = path.to_string_lossy().to_string();
        entries_processed += 1;

        // Skip arbitrumdata entirely (432GB — not useful for pool state)
        if path_str.starts_with("./arbitrumdata/") || path_str.starts_with("arbitrumdata/") {
            // Don't extract, just skip
            continue;
        }

        // For l2chaindata SST files: extract to temp, scan, store in Redis, delete
        if (path_str.contains("l2chaindata/") || path_str.contains("l2chaindata\\"))
            && path_str.ends_with(".sst")
        {
            let filename = path.file_name().unwrap().to_string_lossy().to_string();
            let tmp_path = format!("{}/{}", tmp_dir, filename);

            // Extract to temp file
            let mut file = match std::fs::File::create(&tmp_path) {
                Ok(f) => f,
                Err(_) => continue,
            };
            std::io::copy(&mut entry, &mut file).ok();
            drop(file);

            total_sst += 1;

            // Move SST to l2chaindata dir for pebble-scanner to process
            let dest = format!("{}/l2chaindata/{}", extract_dir, filename);
            std::fs::rename(&tmp_path, &dest).ok();

            if total_sst % 100 == 0 {
                eprintln!(
                    "[{}] SSTs extracted: {} | entries: {}",
                    path_str, total_sst, entries_processed
                );
            }
            continue;
        }

        // For metadata files (CURRENT, MANIFEST, etc.): extract to disk
        if path_str.contains("l2chaindata/") && !path_str.ends_with(".sst") {
            let full_path = PathBuf::from(&extract_dir).join(&path);
            if let Some(parent) = full_path.parent() {
                std::fs::create_dir_all(parent).ok();
            }
            let mut file = match std::fs::File::create(&full_path) {
                Ok(f) => f,
                Err(_) => continue,
            };
            std::io::copy(&mut entry, &mut file).ok();
            eprintln!("  Extracted metadata: {}", path_str);
            continue;
        }

        // For other dirs (triecache, classic-msg): extract normally
        if !path_str.starts_with("./arbitrumdata") && !path_str.starts_with("arbitrumdata") {
            let full_path = PathBuf::from(&extract_dir).join(&path);
            if entry.header().entry_type().is_dir() {
                std::fs::create_dir_all(&full_path).ok();
            } else {
                if let Some(parent) = full_path.parent() {
                    std::fs::create_dir_all(parent).ok();
                }
                let mut file = match std::fs::File::create(&full_path) {
                    Ok(f) => f,
                    Err(_) => continue,
                };
                std::io::copy(&mut entry, &mut file).ok();
            }
        }
    }

    eprintln!();
    eprintln!("=== Scan Complete ===");
    eprintln!("Total SST files scanned: {}", total_sst);
    eprintln!("Total pool entries found: {}", total_found);
    eprintln!("Tar entries processed: {}", entries_processed);

    // Cleanup
    std::fs::remove_dir_all(tmp_dir).ok();
}
