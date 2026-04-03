use alloy::primitives::Address;
use eyre::Result;
use futures_util::{SinkExt, StreamExt};
use tokio::sync::mpsc;
use tokio_tungstenite::{connect_async, tungstenite::Message};
use tracing::{debug, error, info, warn};

use crate::decoder::{decode_swap, DecodedSwap};

/// Typed JSON structs for fast deserialization (avoids serde_json::Value overhead)
#[derive(serde::Deserialize)]
struct FeedBatch {
    messages: Option<Vec<FeedMessage>>,
}

#[derive(serde::Deserialize)]
struct FeedMessage {
    message: Option<FeedInner>,
}

#[derive(serde::Deserialize)]
struct FeedInner {
    message: Option<FeedL2>,
}

#[derive(serde::Deserialize)]
struct FeedL2 {
    #[serde(rename = "l2Msg")]
    l2_msg: Option<String>,
}

/// Raw transaction data received from the sequencer feed
#[derive(Debug, Clone)]
pub struct SequencerTx {
    /// Raw RLP-encoded transaction bytes
    pub data: Vec<u8>,
    /// Sequence number assigned by the sequencer
    pub seq_num: u64,
}

/// Connects to the Arbitrum Sequencer Feed and streams ordered transactions.
///
/// The sequencer feed delivers transactions in the order they will be included,
/// giving us a ~250ms window to react with backrun arbitrage.
pub async fn start_feed(
    feed_url: &str,
    tx_sender: mpsc::UnboundedSender<SequencerTx>,
) -> Result<()> {
    info!(url = feed_url, "Connecting to sequencer feed");

    loop {
        match connect_and_stream(feed_url, &tx_sender).await {
            Ok(()) => {
                warn!("Sequencer feed connection closed, reconnecting...");
            }
            Err(e) => {
                error!(error = %e, "Sequencer feed error, reconnecting in 2s...");
                tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            }
        }
    }
}

async fn connect_and_stream(
    feed_url: &str,
    tx_sender: &mpsc::UnboundedSender<SequencerTx>,
) -> Result<()> {
    let (ws_stream, _) = connect_async(feed_url).await?;
    let (mut write, mut read) = ws_stream.split();

    info!("Connected to sequencer feed");

    // Send initial subscription message (Arbitrum feed protocol)
    let subscribe_msg = serde_json::json!({
        "version": 1,
        "messages": []
    });
    write
        .send(Message::Text(subscribe_msg.to_string().into()))
        .await?;

    while let Some(msg) = read.next().await {
        let msg = msg?;
        match msg {
            Message::Text(text) => {
                if let Err(e) = process_feed_message(&text, tx_sender) {
                    warn!(error = %e, "Failed to process feed message");
                }
            }
            Message::Binary(data) => {
                if let Err(e) = process_feed_binary(&data, tx_sender) {
                    warn!(error = %e, "Failed to process binary feed message");
                }
            }
            Message::Ping(_) => {}
            Message::Pong(_) => {}
            Message::Close(_) => break,
            _ => {}
        }
    }

    Ok(())
}

fn process_feed_message(
    text: &str,
    tx_sender: &mpsc::UnboundedSender<SequencerTx>,
) -> Result<()> {
    let parsed: serde_json::Value = serde_json::from_str(text)?;

    // Arbitrum feed sends messages with a "messages" array
    if let Some(messages) = parsed.get("messages").and_then(|m| m.as_array()) {
        for msg in messages {
            if let Some(seq_num) = msg
                .get("sequenceNumber")
                .and_then(|s| s.as_u64())
            {
                // The message contains the sequenced transaction data
                if let Some(data_hex) = msg
                    .pointer("/message/message/data")
                    .and_then(|d| d.as_str())
                {
                    let data_hex = data_hex.strip_prefix("0x").unwrap_or(data_hex);
                    if let Ok(data) = hex::decode(data_hex) {
                        let _ = tx_sender.send(SequencerTx { data, seq_num });
                    }
                }
            }
        }
    }

    Ok(())
}

fn process_feed_binary(
    data: &[u8],
    tx_sender: &mpsc::UnboundedSender<SequencerTx>,
) -> Result<()> {
    // Binary feed messages - attempt to parse as JSON first
    if let Ok(text) = std::str::from_utf8(data) {
        return process_feed_message(text, tx_sender);
    }

    // Raw binary transaction data
    let _ = tx_sender.send(SequencerTx {
        data: data.to_vec(),
        seq_num: 0,
    });

    Ok(())
}

/// Start sequencer feed with 3 parallel connections for redundancy.
/// Fastest connection delivers the swap first — dedup by pool+token_in.
pub async fn start_feed_scanner(
    feed_url: &str,
    swap_sender: mpsc::UnboundedSender<DecodedSwap>,
) -> Result<()> {
    info!(url = feed_url, "Connecting sequencer feed scanner (3 connections)");

    // Spawn 3 parallel feed connections — first to deliver wins
    let url = feed_url.to_string();
    for i in 0..3 {
        let u = url.clone();
        let tx = swap_sender.clone();
        tokio::spawn(async move {
            loop {
                match feed_scan_loop(&u, &tx).await {
                    Ok(()) => warn!(conn = i, "Feed connection closed, reconnecting..."),
                    Err(e) => {
                        warn!(conn = i, error = %e, "Feed error, reconnecting in 1s...");
                        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                    }
                }
            }
        });
    }

    // Keep this task alive
    loop {
        tokio::time::sleep(std::time::Duration::from_secs(3600)).await;
    }
}

async fn feed_scan_loop(
    feed_url: &str,
    swap_sender: &mpsc::UnboundedSender<DecodedSwap>,
) -> Result<()> {
    // Build WebSocket request with Arbitrum Feed v2 headers
    // This ensures we get the latest protocol format
    use tokio_tungstenite::tungstenite::http;
    let request = http::Request::builder()
        .uri(feed_url)
        .header("Connection", "Upgrade")
        .header("Upgrade", "websocket")
        .header("Sec-WebSocket-Version", "13")
        .header("Sec-WebSocket-Key", tokio_tungstenite::tungstenite::handshake::client::generate_key())
        .header("Host", feed_url.split("//").last().unwrap_or("arb1.arbitrum.io").split('/').next().unwrap_or("arb1.arbitrum.io"))
        .header("Arbitrum-Feed-Client-Version", "2")
        .header("Arbitrum-Requested-Sequence-number", "0")
        .body(())?;

    let (ws_stream, _) = connect_async(request).await?;
    let (mut write, mut read) = ws_stream.split();

    info!("Feed scanner connected (v2 headers)");

    while let Some(msg) = read.next().await {
        let msg = msg?;
        let text = match msg {
            Message::Text(t) => t.to_string(),
            Message::Binary(d) => match std::str::from_utf8(&d) {
                Ok(s) => s.to_string(),
                Err(_) => continue,
            },
            Message::Ping(_) | Message::Pong(_) => continue,
            Message::Close(_) => break,
            _ => continue,
        };

        // Fast typed deserialization (avoids serde_json::Value overhead)
        let batch: FeedBatch = match serde_json::from_str(&text) {
            Ok(b) => b,
            Err(e) => {
                debug!(len = text.len(), error = %e, preview = &text[..text.len().min(200)], "Feed parse failed");
                continue;
            }
        };

        let messages = match batch.messages {
            Some(m) if !m.is_empty() => m,
            _ => continue,
        };

        for msg in &messages {
            let l2_msg = match msg.message.as_ref()
                .and_then(|m| m.message.as_ref())
                .and_then(|m| m.l2_msg.as_deref())
            {
                Some(s) => s,
                None => continue,
            };

            // Base64 decode
            use base64::Engine;
            let raw = match base64::engine::general_purpose::STANDARD.decode(l2_msg) {
                Ok(d) if !d.is_empty() => d,
                _ => continue,
            };

            // Decode tx and check for swap
            if let Some(swap) = try_decode_tx_swap(&raw) {
                info!(dex = ?swap.dex, token_in = %swap.token_in, "FEED swap");
                let _ = swap_sender.send(swap);
            }
        }
    }

    Ok(())
}

/// Try to decode a raw sequencer message into a swap.
fn try_decode_tx_swap(raw: &[u8]) -> Option<DecodedSwap> {
    // Arbitrum L2 message: first byte is message type (03=L2, 04=L2 signed)
    // After that comes the RLP-encoded transaction
    if raw.len() < 10 {
        return None;
    }

    // Skip Arbitrum message header (1 byte type)
    let tx_data = &raw[1..];

    use alloy::consensus::{TxEnvelope, Transaction};
    use alloy::rlp::Decodable;

    let envelope = TxEnvelope::decode(&mut &tx_data[..]).ok()?;

    let (to, input) = match &envelope {
        TxEnvelope::Legacy(signed) => {
            let tx = signed.tx();
            (tx.to()?, tx.input().to_vec())
        }
        TxEnvelope::Eip2930(signed) => {
            let tx = signed.tx();
            (tx.to()?, tx.input().to_vec())
        }
        TxEnvelope::Eip1559(signed) => {
            let tx = signed.tx();
            (tx.to()?, tx.input().to_vec())
        }
        _ => return None,
    };

    decode_swap(&to, &input, Address::ZERO)
}
