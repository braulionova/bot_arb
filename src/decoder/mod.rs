use alloy::primitives::{Address, U256};

use tracing::debug;

/// Decoded swap event from a DEX transaction
#[derive(Debug, Clone)]
pub struct DecodedSwap {
    /// DEX that executed the swap
    pub dex: DexType,
    /// Pool address where the swap occurred
    pub pool: Address,
    /// Token being sold
    pub token_in: Address,
    /// Token being bought
    pub token_out: Address,
    /// Amount of token_in
    pub amount_in: U256,
    /// Amount of token_out
    pub amount_out: U256,
    /// The sender of the swap tx
    pub sender: Address,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DexType {
    UniswapV3,
    UniswapV2,
    CamelotV3,
    CamelotV2,
    SushiSwapV3,
    SushiSwapV2,
    PancakeSwapV3,
    RamsesV2,
    CurveStable,
    BalancerStable,
    Unknown,
}

// Well-known function selectors for swap detection
const UNISWAP_V3_EXACT_INPUT_SINGLE: [u8; 4] = [0x41, 0x4b, 0xf3, 0x89]; // exactInputSingle
const UNISWAP_V3_EXACT_INPUT: [u8; 4] = [0xc0, 0x4b, 0x8d, 0x59]; // exactInput
const UNISWAP_V3_EXACT_OUTPUT_SINGLE: [u8; 4] = [0xdb, 0x3e, 0x21, 0x98]; // exactOutputSingle
const UNISWAP_V3_EXACT_OUTPUT: [u8; 4] = [0xf2, 0x8c, 0x05, 0x98]; // exactOutput
const UNISWAP_V2_SWAP_EXACT_TOKENS: [u8; 4] = [0x38, 0xed, 0x17, 0x39]; // swapExactTokensForTokens
const UNISWAP_V2_SWAP_TOKENS_EXACT: [u8; 4] = [0x8a, 0x65, 0x7e, 0x67]; // swapTokensForExactTokens
const UNISWAP_V2_SWAP_EXACT_ETH: [u8; 4] = [0x7f, 0xf3, 0x6a, 0xb5]; // swapExactETHForTokens
const UNISWAP_V2_SWAP_EXACT_TOKENS_ETH: [u8; 4] = [0x18, 0xcb, 0xaf, 0xe5]; // swapExactTokensForETH
// Universal Router / multicall selectors
const UNIVERSAL_EXECUTE: [u8; 4] = [0x35, 0x93, 0xd1, 0x27]; // execute(bytes,bytes[],uint256)
const UNIVERSAL_EXECUTE_NO_DL: [u8; 4] = [0x24, 0x85, 0x6b, 0xc3]; // execute(bytes,bytes[])
const MULTICALL: [u8; 4] = [0xac, 0x96, 0x50, 0xd8]; // multicall(uint256,bytes[])
const MULTICALL_NO_DL: [u8; 4] = [0x5a, 0xe4, 0x01, 0xdc]; // multicall(bytes[])

/// Known router addresses on Arbitrum
pub struct KnownRouters;

impl KnownRouters {
    // ─── Uniswap ───
    pub const UNISWAP_V3_ROUTER: [u8; 20] = hex_literal("E592427A0AEce92De3Edee1F18E0157C05861564");
    pub const UNISWAP_V3_ROUTER02: [u8; 20] = hex_literal("68b3465833fb72A70ecDF485E0e4C7bD8665Fc45");
    pub const UNISWAP_V2_ROUTER: [u8; 20] = hex_literal("4752ba5DBc23f44D87826276BF6Fd6b1C372aD24");
    pub const UNISWAP_UNIVERSAL: [u8; 20] = hex_literal("4C60051384bd2d3C01bfc845Cf5F4b44bcbE9de5");
    // ─── Camelot ───
    pub const CAMELOT_V3_ROUTER: [u8; 20] = hex_literal("1F721E2E82F6676FCE4eA07A5958cF098D339e18");
    pub const CAMELOT_V2_ROUTER: [u8; 20] = hex_literal("c873fEcbd354f5A56E00E710B90EF4201db2448d");
    // ─── SushiSwap ───
    pub const SUSHI_V2_ROUTER: [u8; 20] = hex_literal("1b02dA8Cb0d097eB8D57A175b88c7D8b47997506");
    pub const SUSHI_ROUTE_PROC: [u8; 20] = hex_literal("09bD2A33c47746fF03b86BCe4E885D03C74a8E8C");
    // ─── PancakeSwap ───
    pub const PANCAKE_V3_ROUTER: [u8; 20] = hex_literal("32226588378236Fd0c7c4053999F88aC0e5cAc77");
    // ─── Ramses ───
    pub const RAMSES_V3_ROUTER: [u8; 20] = hex_literal("4730e03EB4a58A5e20244062D5f9A99bCf5770a6");
    // ─── Zyberswap ───
    pub const ZYBER_ROUTER: [u8; 20] = hex_literal("16e71B13fE6079B4312063F7E81F76d165Ad32Ad");
    // ─── Chronos ───
    pub const CHRONOS_ROUTER: [u8; 20] = hex_literal("e708aA9E887980750C040a6A2Cb901c37AA34F3b");
    // ─── TraderJoe ───
    pub const TRADERJOE_LB_ROUTER: [u8; 20] = hex_literal("b4315e873dBcf96Ffd0acd8EA43f689D8c20fB30");
}

const fn hex_literal(s: &str) -> [u8; 20] {
    let bytes = s.as_bytes();
    let mut result = [0u8; 20];
    let mut i = 0;
    while i < 20 {
        let hi = hex_nibble(bytes[i * 2]);
        let lo = hex_nibble(bytes[i * 2 + 1]);
        result[i] = (hi << 4) | lo;
        i += 1;
    }
    result
}

const fn hex_nibble(b: u8) -> u8 {
    match b {
        b'0'..=b'9' => b - b'0',
        b'a'..=b'f' => b - b'a' + 10,
        b'A'..=b'F' => b - b'A' + 10,
        _ => 0,
    }
}

/// Attempts to decode a swap from raw transaction calldata.
/// Returns None if the tx is not a recognized swap.
pub fn decode_swap(to: &Address, calldata: &[u8], sender: Address) -> Option<DecodedSwap> {
    if calldata.len() < 4 {
        return None;
    }

    let selector: [u8; 4] = calldata[0..4].try_into().ok()?;
    let to_bytes: [u8; 20] = to.0.into();

    let dex = identify_dex(&to_bytes);
    if dex == DexType::Unknown {
        return None;
    }

    let swap = match selector {
        UNISWAP_V3_EXACT_INPUT_SINGLE => decode_v3_exact_input_single(calldata, dex, sender),
        UNISWAP_V3_EXACT_INPUT => decode_v3_exact_input(calldata, dex, sender),
        UNISWAP_V3_EXACT_OUTPUT_SINGLE => decode_v3_exact_output_single(calldata, dex, sender),
        UNISWAP_V3_EXACT_OUTPUT => decode_v3_exact_output(calldata, dex, sender),
        UNISWAP_V2_SWAP_EXACT_TOKENS => decode_v2_swap(calldata, dex, sender),
        UNISWAP_V2_SWAP_TOKENS_EXACT => decode_v2_swap(calldata, dex, sender),
        UNISWAP_V2_SWAP_EXACT_ETH => decode_v2_swap(calldata, dex, sender),
        UNISWAP_V2_SWAP_EXACT_TOKENS_ETH => decode_v2_swap(calldata, dex, sender),
        // Universal Router: execute(bytes,bytes[],uint256) and execute(bytes,bytes[])
        UNIVERSAL_EXECUTE | UNIVERSAL_EXECUTE_NO_DL => decode_universal_router(calldata, dex, sender),
        // multicall: try decoding inner calls
        MULTICALL | MULTICALL_NO_DL => decode_multicall(calldata, dex, sender),
        _ => None,
    };

    if swap.is_some() {
        debug!(?dex, "Decoded swap from tx");
    }

    swap
}

fn identify_dex(to: &[u8; 20]) -> DexType {
    match to {
        // Uniswap V3 (standard V3 selectors)
        x if x == &KnownRouters::UNISWAP_V3_ROUTER => DexType::UniswapV3,
        x if x == &KnownRouters::UNISWAP_V3_ROUTER02 => DexType::UniswapV3,
        x if x == &KnownRouters::UNISWAP_UNIVERSAL => DexType::UniswapV3,
        // Uniswap V2
        x if x == &KnownRouters::UNISWAP_V2_ROUTER => DexType::UniswapV2,
        // Camelot
        x if x == &KnownRouters::CAMELOT_V3_ROUTER => DexType::CamelotV3,
        x if x == &KnownRouters::CAMELOT_V2_ROUTER => DexType::CamelotV2,
        // SushiSwap
        x if x == &KnownRouters::SUSHI_V2_ROUTER => DexType::SushiSwapV2,
        x if x == &KnownRouters::SUSHI_ROUTE_PROC => DexType::SushiSwapV3,
        // PancakeSwap
        x if x == &KnownRouters::PANCAKE_V3_ROUTER => DexType::PancakeSwapV3,
        // Ramses (V3 fork — same selectors)
        x if x == &KnownRouters::RAMSES_V3_ROUTER => DexType::UniswapV3,
        // Zyberswap (V2 fork)
        x if x == &KnownRouters::ZYBER_ROUTER => DexType::UniswapV2,
        // Chronos (Solidly/V2 fork)
        x if x == &KnownRouters::CHRONOS_ROUTER => DexType::UniswapV2,
        // TraderJoe (V2-like selectors)
        x if x == &KnownRouters::TRADERJOE_LB_ROUTER => DexType::UniswapV2,
        _ => DexType::Unknown,
    }
}

/// Decode UniswapV3 exactInputSingle(ExactInputSingleParams)
/// Params struct: tokenIn, tokenOut, fee, recipient, amountIn, amountOutMinimum, sqrtPriceLimitX96
fn decode_v3_exact_input_single(calldata: &[u8], dex: DexType, sender: Address) -> Option<DecodedSwap> {
    // Skip 4-byte selector, then ABI-decode params
    if calldata.len() < 4 + 7 * 32 {
        return None;
    }
    let params = &calldata[4..];

    let token_in = address_from_word(&params[0..32]);
    let token_out = address_from_word(&params[32..64]);
    // skip fee (64..96), recipient (96..128)
    let amount_in = U256::from_be_slice(&params[128..160]);
    let amount_out_min = U256::from_be_slice(&params[160..192]);

    Some(DecodedSwap {
        dex,
        pool: Address::ZERO, // Resolved later via pool lookup
        token_in,
        token_out,
        amount_in,
        amount_out: amount_out_min,
        sender,
    })
}

/// Decode UniswapV3 exactInput (multi-hop path encoded)
fn decode_v3_exact_input(calldata: &[u8], dex: DexType, sender: Address) -> Option<DecodedSwap> {
    if calldata.len() < 4 + 5 * 32 {
        return None;
    }
    let params = &calldata[4..];

    // ExactInputParams: path (bytes), recipient, amountIn, amountOutMinimum
    // path offset is at params[0..32]
    let path_offset = U256::from_be_slice(&params[0..32]).to::<usize>();
    let amount_in = U256::from_be_slice(&params[96..128]);
    let amount_out_min = U256::from_be_slice(&params[128..160]);

    // Decode path: first 20 bytes = tokenIn, then 3 bytes fee, then 20 bytes tokenOut, ...
    if params.len() < path_offset + 32 + 23 {
        return None;
    }
    let path_len = U256::from_be_slice(&params[path_offset..path_offset + 32]).to::<usize>();
    let path_data = &params[path_offset + 32..path_offset + 32 + path_len];

    if path_data.len() < 43 {
        return None;
    }

    let token_in = Address::from_slice(&path_data[0..20]);
    // Last 20 bytes of path = final token_out
    let token_out = Address::from_slice(&path_data[path_data.len() - 20..]);

    Some(DecodedSwap {
        dex,
        pool: Address::ZERO,
        token_in,
        token_out,
        amount_in,
        amount_out: amount_out_min,
        sender,
    })
}

fn decode_v3_exact_output_single(calldata: &[u8], dex: DexType, sender: Address) -> Option<DecodedSwap> {
    if calldata.len() < 4 + 7 * 32 {
        return None;
    }
    let params = &calldata[4..];

    let token_in = address_from_word(&params[0..32]);
    let token_out = address_from_word(&params[32..64]);
    let amount_out = U256::from_be_slice(&params[128..160]);
    let amount_in_max = U256::from_be_slice(&params[160..192]);

    Some(DecodedSwap {
        dex,
        pool: Address::ZERO,
        token_in,
        token_out,
        amount_in: amount_in_max,
        amount_out,
        sender,
    })
}

fn decode_v3_exact_output(calldata: &[u8], dex: DexType, sender: Address) -> Option<DecodedSwap> {
    if calldata.len() < 4 + 4 * 32 {
        return None;
    }
    let params = &calldata[4..];

    let path_offset = U256::from_be_slice(&params[0..32]).to::<usize>();
    let amount_out = U256::from_be_slice(&params[96..128]);
    let amount_in_max = U256::from_be_slice(&params[128..160]);

    if params.len() < path_offset + 32 + 23 {
        return None;
    }
    let path_len = U256::from_be_slice(&params[path_offset..path_offset + 32]).to::<usize>();
    let path_data = &params[path_offset + 32..path_offset + 32 + path_len];

    if path_data.len() < 43 {
        return None;
    }

    // Note: in exactOutput, path is reversed (tokenOut first)
    let token_out = Address::from_slice(&path_data[0..20]);
    let token_in = Address::from_slice(&path_data[path_data.len() - 20..]);

    Some(DecodedSwap {
        dex,
        pool: Address::ZERO,
        token_in,
        token_out,
        amount_in: amount_in_max,
        amount_out,
        sender,
    })
}

/// Decode V2-style swap (swapExactTokensForTokens and variants)
fn decode_v2_swap(calldata: &[u8], dex: DexType, sender: Address) -> Option<DecodedSwap> {
    if calldata.len() < 4 + 4 * 32 {
        return None;
    }
    let params = &calldata[4..];

    let amount_in = U256::from_be_slice(&params[0..32]);
    let amount_out_min = U256::from_be_slice(&params[32..64]);

    // path is a dynamic array: offset at params[64..96]
    let path_offset = U256::from_be_slice(&params[64..96]).to::<usize>();
    if params.len() < path_offset + 32 {
        return None;
    }
    let path_len = U256::from_be_slice(&params[path_offset..path_offset + 32]).to::<usize>();
    if path_len < 2 || params.len() < path_offset + 32 + path_len * 32 {
        return None;
    }

    let token_in = address_from_word(&params[path_offset + 32..path_offset + 64]);
    let token_out = address_from_word(
        &params[path_offset + 32 + (path_len - 1) * 32..path_offset + 64 + (path_len - 1) * 32],
    );

    Some(DecodedSwap {
        dex,
        pool: Address::ZERO,
        token_in,
        token_out,
        amount_in,
        amount_out: amount_out_min,
        sender,
    })
}

fn address_from_word(word: &[u8]) -> Address {
    // ABI-encoded address is right-aligned in 32-byte word
    Address::from_slice(&word[12..32])
}

/// Decode Universal Router execute(bytes commands, bytes[] inputs, uint256 deadline)
/// or execute(bytes commands, bytes[] inputs).
/// Each byte in `commands` is a command type; the corresponding element in `inputs`
/// holds the ABI-encoded parameters for that command.
/// We scan for the first swap command and decode it.
fn decode_universal_router(calldata: &[u8], dex: DexType, sender: Address) -> Option<DecodedSwap> {
    if calldata.len() < 4 + 64 {
        return None;
    }
    let params = &calldata[4..];

    // Layout: offset_commands (32) | offset_inputs (32) | [deadline (32)]
    let commands_offset = U256::from_be_slice(&params[0..32]).to::<usize>();
    let inputs_offset = U256::from_be_slice(&params[32..64]).to::<usize>();

    // Decode commands bytes
    if params.len() < commands_offset + 32 {
        return None;
    }
    let commands_len = U256::from_be_slice(&params[commands_offset..commands_offset + 32]).to::<usize>();
    if commands_len == 0 || params.len() < commands_offset + 32 + commands_len {
        return None;
    }
    let commands_data = &params[commands_offset + 32..commands_offset + 32 + commands_len];

    // Decode inputs array
    if params.len() < inputs_offset + 32 {
        return None;
    }
    let inputs_len = U256::from_be_slice(&params[inputs_offset..inputs_offset + 32]).to::<usize>();
    if inputs_len == 0 || inputs_len > 20 || inputs_len != commands_len {
        return None;
    }

    // Scan commands for the first swap command
    for i in 0..commands_len {
        // Lower 5 bits determine the command type (bit 7 is "allow revert" flag)
        let cmd = commands_data[i] & 0x1f;
        let is_swap = matches!(cmd, 0x00 | 0x01 | 0x08 | 0x09);
        if !is_swap {
            continue;
        }

        // Read offset for this input element
        let offset_pos = inputs_offset + 32 + i * 32;
        if params.len() < offset_pos + 32 {
            continue;
        }
        let item_offset = inputs_offset + 32
            + U256::from_be_slice(&params[offset_pos..offset_pos + 32]).to::<usize>();
        if params.len() < item_offset + 32 {
            continue;
        }
        let item_len = U256::from_be_slice(&params[item_offset..item_offset + 32]).to::<usize>();
        if params.len() < item_offset + 32 + item_len {
            continue;
        }
        let input = &params[item_offset + 32..item_offset + 32 + item_len];

        match cmd {
            // V3_SWAP_EXACT_IN: abi.encode(address recipient, uint256 amountIn, uint256 amountOutMin, bytes path, bool payerIsUser)
            0x00 => {
                if let Some(s) = decode_ur_v3_swap_exact_in(input, dex, sender) {
                    return Some(s);
                }
            }
            // V3_SWAP_EXACT_OUT: abi.encode(address recipient, uint256 amountOut, uint256 amountInMax, bytes path, bool payerIsUser)
            0x01 => {
                if let Some(s) = decode_ur_v3_swap_exact_out(input, dex, sender) {
                    return Some(s);
                }
            }
            // V2_SWAP_EXACT_IN: abi.encode(address recipient, uint256 amountIn, uint256 amountOutMin, address[] path, bool payerIsUser)
            0x08 => {
                if let Some(s) = decode_ur_v2_swap(input, dex, sender, true) {
                    return Some(s);
                }
            }
            // V2_SWAP_EXACT_OUT: abi.encode(address recipient, uint256 amountOut, uint256 amountInMax, address[] path, bool payerIsUser)
            0x09 => {
                if let Some(s) = decode_ur_v2_swap(input, dex, sender, false) {
                    return Some(s);
                }
            }
            _ => {}
        }
    }

    None
}

/// Decode V3_SWAP_EXACT_IN from Universal Router input bytes.
/// ABI layout: recipient (32) | amountIn (32) | amountOutMin (32) | path_offset (32) | payerIsUser (32)
fn decode_ur_v3_swap_exact_in(input: &[u8], dex: DexType, sender: Address) -> Option<DecodedSwap> {
    if input.len() < 5 * 32 {
        return None;
    }
    let amount_in = U256::from_be_slice(&input[32..64]);
    let amount_out_min = U256::from_be_slice(&input[64..96]);
    let path_offset = U256::from_be_slice(&input[96..128]).to::<usize>();
    if input.len() < path_offset + 32 {
        return None;
    }
    let path_len = U256::from_be_slice(&input[path_offset..path_offset + 32]).to::<usize>();
    if path_len < 43 || input.len() < path_offset + 32 + path_len {
        return None;
    }
    let path = &input[path_offset + 32..path_offset + 32 + path_len];
    let token_in = Address::from_slice(&path[0..20]);
    let token_out = Address::from_slice(&path[path.len() - 20..]);

    Some(DecodedSwap {
        dex,
        pool: Address::ZERO,
        token_in,
        token_out,
        amount_in,
        amount_out: amount_out_min,
        sender,
    })
}

/// Decode V3_SWAP_EXACT_OUT from Universal Router input bytes.
/// ABI layout: recipient (32) | amountOut (32) | amountInMax (32) | path_offset (32) | payerIsUser (32)
fn decode_ur_v3_swap_exact_out(input: &[u8], dex: DexType, sender: Address) -> Option<DecodedSwap> {
    if input.len() < 5 * 32 {
        return None;
    }
    let amount_out = U256::from_be_slice(&input[32..64]);
    let amount_in_max = U256::from_be_slice(&input[64..96]);
    let path_offset = U256::from_be_slice(&input[96..128]).to::<usize>();
    if input.len() < path_offset + 32 {
        return None;
    }
    let path_len = U256::from_be_slice(&input[path_offset..path_offset + 32]).to::<usize>();
    if path_len < 43 || input.len() < path_offset + 32 + path_len {
        return None;
    }
    let path = &input[path_offset + 32..path_offset + 32 + path_len];
    // In exactOutput, the V3 path is reversed: tokenOut is first, tokenIn is last
    let token_out = Address::from_slice(&path[0..20]);
    let token_in = Address::from_slice(&path[path.len() - 20..]);

    Some(DecodedSwap {
        dex,
        pool: Address::ZERO,
        token_in,
        token_out,
        amount_in: amount_in_max,
        amount_out,
        sender,
    })
}

/// Decode V2_SWAP_EXACT_IN or V2_SWAP_EXACT_OUT from Universal Router input bytes.
/// ABI layout: recipient (32) | amount (32) | amountLimit (32) | path_offset (32) | payerIsUser (32)
/// `exact_in`: if true, amount=amountIn and amountLimit=amountOutMin; else amount=amountOut, amountLimit=amountInMax
fn decode_ur_v2_swap(input: &[u8], dex: DexType, sender: Address, exact_in: bool) -> Option<DecodedSwap> {
    if input.len() < 5 * 32 {
        return None;
    }
    let amount = U256::from_be_slice(&input[32..64]);
    let amount_limit = U256::from_be_slice(&input[64..96]);
    let path_offset = U256::from_be_slice(&input[96..128]).to::<usize>();
    if input.len() < path_offset + 32 {
        return None;
    }
    let path_len = U256::from_be_slice(&input[path_offset..path_offset + 32]).to::<usize>();
    if path_len < 2 || input.len() < path_offset + 32 + path_len * 32 {
        return None;
    }
    let token_in = address_from_word(&input[path_offset + 32..path_offset + 64]);
    let token_out = address_from_word(
        &input[path_offset + 32 + (path_len - 1) * 32..path_offset + 64 + (path_len - 1) * 32],
    );

    let (amount_in, amount_out) = if exact_in {
        (amount, amount_limit)
    } else {
        (amount_limit, amount)
    };

    Some(DecodedSwap {
        dex,
        pool: Address::ZERO,
        token_in,
        token_out,
        amount_in,
        amount_out,
        sender,
    })
}

/// Decode multicall(uint256 deadline, bytes[] data) — SwapRouter02 pattern
/// Extracts the first recognizable swap from the inner calls
fn decode_multicall(calldata: &[u8], dex: DexType, sender: Address) -> Option<DecodedSwap> {
    if calldata.len() < 4 + 32 {
        return None;
    }
    let params = &calldata[4..];

    // multicall(uint256, bytes[]) — skip deadline, get array offset
    // multicall(bytes[]) — array offset directly
    let arr_offset = if calldata[0..4] == MULTICALL {
        // Has deadline: array offset at params[32..64]
        if params.len() < 64 { return None; }
        U256::from_be_slice(&params[32..64]).to::<usize>()
    } else {
        // No deadline: array offset at params[0..32]
        U256::from_be_slice(&params[0..32]).to::<usize>()
    };

    if params.len() < arr_offset + 32 {
        return None;
    }

    let arr_len = U256::from_be_slice(&params[arr_offset..arr_offset + 32]).to::<usize>();
    if arr_len == 0 || arr_len > 10 {
        return None;
    }

    // Try each inner call
    for i in 0..arr_len {
        let item_offset_pos = arr_offset + 32 + i * 32;
        if params.len() < item_offset_pos + 32 {
            break;
        }
        let item_offset = arr_offset + 32 + U256::from_be_slice(&params[item_offset_pos..item_offset_pos + 32]).to::<usize>();
        if params.len() < item_offset + 32 {
            break;
        }
        let item_len = U256::from_be_slice(&params[item_offset..item_offset + 32]).to::<usize>();
        if item_len < 4 || params.len() < item_offset + 32 + item_len {
            continue;
        }
        let inner = &params[item_offset + 32..item_offset + 32 + item_len];

        // Build a fake full calldata (with selector) for the inner call
        let sel: [u8; 4] = inner[0..4].try_into().ok()?;
        let result = match sel {
            UNISWAP_V3_EXACT_INPUT_SINGLE => decode_v3_exact_input_single(inner, dex, sender),
            UNISWAP_V3_EXACT_INPUT => decode_v3_exact_input(inner, dex, sender),
            UNISWAP_V3_EXACT_OUTPUT_SINGLE => decode_v3_exact_output_single(inner, dex, sender),
            UNISWAP_V3_EXACT_OUTPUT => decode_v3_exact_output(inner, dex, sender),
            UNISWAP_V2_SWAP_EXACT_TOKENS => decode_v2_swap(inner, dex, sender),
            UNISWAP_V2_SWAP_TOKENS_EXACT => decode_v2_swap(inner, dex, sender),
            _ => None,
        };
        if result.is_some() {
            return result;
        }
    }

    None
}
