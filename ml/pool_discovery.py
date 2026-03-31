#!/usr/bin/env python3
"""
Pool Discovery — Finds ALL pools across ALL factories on Arbitrum.
Maps which token pairs have 2+ pools on different DEXes (arb candidates).
Outputs: ml/all_pools.json, ml/arb_pairs.json
"""

import json
import subprocess
import time
import sys
from collections import defaultdict

ALL_POOLS_FILE = "/home/ubuntu/arbitrum_bot/ml/all_pools.json"
ARB_PAIRS_FILE = "/home/ubuntu/arbitrum_bot/ml/arb_pairs.json"


def rpc(method, params, url="https://arbitrum.drpc.org"):
    body = json.dumps({"jsonrpc": "2.0", "method": method, "params": params, "id": 1})
    try:
        r = subprocess.run(
            ["curl", "-s", "-m", "5", "-X", "POST", url,
             "-H", "Content-Type: application/json", "-d", body],
            capture_output=True, text=True, timeout=8
        )
        if r.stdout.strip():
            return json.loads(r.stdout)
    except:
        pass
    return {}


def eth_call(to, data):
    r = rpc("eth_call", [{"to": to, "data": data}, "latest"])
    return r.get("result", "0x")


def get_v2_pairs(factory, dex_name, max_pairs=1000):
    """Get all pairs from a V2 factory"""
    # allPairsLength()
    res = eth_call(factory, "0x574f2ba3")
    if not res or res == "0x" or len(res) < 66:
        return []

    total = int(res[2:66], 16)
    total = min(total, max_pairs)
    print(f"  {dex_name}: {total} pairs")

    pools = []
    for i in range(total):
        # allPairs(i)
        idx = hex(i)[2:].zfill(64)
        pair_addr_raw = eth_call(factory, "0x1e3dd18b" + idx)
        if not pair_addr_raw or len(pair_addr_raw) < 66:
            continue
        pair_addr = "0x" + pair_addr_raw[26:66]
        if pair_addr == "0x" + "0" * 40:
            continue

        # token0(), token1()
        t0 = eth_call(pair_addr, "0x0dfe1681")
        t1 = eth_call(pair_addr, "0xd21220a7")
        if not t0 or not t1 or len(t0) < 66 or len(t1) < 66:
            continue

        token0 = "0x" + t0[26:66]
        token1 = "0x" + t1[26:66]

        # getReserves() — check if pool has liquidity
        reserves = eth_call(pair_addr, "0x0902f1ac")
        if reserves and len(reserves) >= 130:
            r0 = int(reserves[2:66], 16)
            r1 = int(reserves[66:130], 16)
            if r0 == 0 or r1 == 0:
                continue
        else:
            continue

        pools.append({
            "address": pair_addr.lower(),
            "dex": dex_name,
            "type": "v2",
            "token0": token0.lower(),
            "token1": token1.lower(),
            "reserve0": r0,
            "reserve1": r1,
        })

        if (i + 1) % 50 == 0:
            sys.stdout.write(f"\r    {dex_name}: {i+1}/{total} ({len(pools)} with liquidity)")
            sys.stdout.flush()
            time.sleep(0.5)

    print(f"\r    {dex_name}: {len(pools)} pools with liquidity (of {total})")
    return pools


def get_v3_pools(factory, dex_name, tokens, fees):
    """Get V3 pools for all token pair + fee combinations"""
    pools = []
    checked = 0

    for i in range(len(tokens)):
        for j in range(i + 1, len(tokens)):
            for fee in fees:
                # getPool(tokenA, tokenB, fee)
                ta = tokens[i][0][2:].zfill(64)
                tb = tokens[j][0][2:].zfill(64)
                fee_hex = hex(fee)[2:].zfill(64)
                data = "0x1698ee82" + ta + tb + fee_hex

                res = eth_call(factory, data)
                if not res or len(res) < 66:
                    continue

                pool_addr = "0x" + res[26:66]
                if pool_addr == "0x" + "0" * 40:
                    continue

                # Check liquidity
                liq = eth_call(pool_addr, "0x1a686502")
                if liq and len(liq) >= 66:
                    liquidity = int(liq[2:66], 16)
                    if liquidity == 0:
                        continue
                else:
                    continue

                pools.append({
                    "address": pool_addr.lower(),
                    "dex": dex_name,
                    "type": "v3",
                    "token0": tokens[i][0].lower(),
                    "token1": tokens[j][0].lower(),
                    "fee": fee,
                    "liquidity": liquidity,
                })

                checked += 1
                if checked % 20 == 0:
                    time.sleep(0.3)

    print(f"    {dex_name}: {len(pools)} V3 pools")
    return pools


def discover_all():
    """Discover all pools across all factories"""
    print("=== Pool Discovery ===\n")

    all_pools = []

    # V2 Factories
    v2_factories = [
        ("0x6EcCab422D763aC031210895C81787E87B43A652", "CamelotV2", 1000),
        ("0xc35DADB65012eC5796536bD9864eD8773aBc74C4", "SushiSwapV2", 800),
        ("0xf1D7CC64Fb4452F05c498126312eBE29f30Fbcf9", "UniswapV2", 500),
        ("0xAAA20D08e59F6561f242b08513D36266C5A29415", "RamsesV2", 300),
        ("0xaE4EC9901c3076D0DdBe76A520F9E90a6227aCB7", "TraderJoeV1", 200),
        ("0xaC2ee06A14c52570Ef3B9812Ed240BCe359772e7", "ZyberswapV2", 100),
    ]

    print("Scanning V2 factories:")
    for factory, name, max_p in v2_factories:
        try:
            pools = get_v2_pairs(factory, name, max_p)
            all_pools.extend(pools)
        except Exception as e:
            print(f"  Error on {name}: {e}")
        time.sleep(1)

    # V3 Factories
    TOKENS = [
        ("0x82aF49447D8a07e3bd95BD0d56f35241523fBab1", "WETH"),
        ("0xaf88d065e77c8cC2239327C5EDb3A432268e5831", "USDC"),
        ("0xFF970A61A04b1cA14834A43f5dE4533eBDDB5CC8", "USDC.e"),
        ("0xFd086bC7CD5C481DCC9C85ebE478A1C0b69FCbb9", "USDT"),
        ("0x912CE59144191C1204E64559FE8253a0e49E6548", "ARB"),
        ("0x2f2a2543B76A4166549F7aaB2e75Bef0aefC5B0f", "WBTC"),
        ("0xfc5A1A6EB076a2C7aD06eD22C90d7E710E35ad0a", "GMX"),
        ("0xf97f4df75117a78c1A5a0DBb814Af92458539FB4", "LINK"),
        ("0x539bdE0d7Dbd336b79148AA742883198BBF60342", "MAGIC"),
        ("0xDA10009cBd5D07dd0CeCc66161FC93D7c9000da1", "DAI"),
        ("0x0c880f6761F1af8d9Aa9C466984b80DAb9a8c9e8", "PENDLE"),
        ("0x3082CC23568eA640225c2467653dB90e9250AaA0", "RDNT"),
        ("0x6C2C06790b3E3E3c38e12Ee22F8183b37a13EE55", "DPX"),
        ("0x3d9907F9a368ad0a51Be60f7Da3b97cf940982D8", "GRAIL"),
        ("0x5979D7b546E38E414F7E9822514be443A4800529", "wstETH"),
        ("0x6694340fc020c5E6B96567843da2df01b2CE1eb6", "STG"),
        ("0x10393c20975cF177a3513071bC110f7962CD67da", "JONES"),
        ("0xFa7F8980b0f1E64A2062791cc3b0871572f1F7f0", "UNI"),
        ("0x17FC002b466eEc40DaE837Fc4bE5c67993ddBd6F", "FRAX"),
    ]
    FEES = [100, 500, 3000, 10000]

    v3_factories = [
        ("0x1F98431c8aD98523631AE4a59f267346ea31F984", "UniswapV3"),
        ("0x0BFbCF9fa4f9C56B0F40a671Ad40E0805A091865", "PancakeSwapV3"),
        ("0x1af415a1EbA07a4986a52B6f2e7dE7003D82231e", "SushiSwapV3"),
    ]

    print("\nScanning V3 factories:")
    for factory, name in v3_factories:
        try:
            pools = get_v3_pools(factory, name, TOKENS, FEES)
            all_pools.extend(pools)
        except Exception as e:
            print(f"  Error on {name}: {e}")
        time.sleep(1)

    # Save all pools
    with open(ALL_POOLS_FILE, "w") as f:
        json.dump(all_pools, f, indent=2)
    print(f"\nTotal pools: {len(all_pools)}")

    # Find arb pairs: token pairs with 2+ pools on different DEXes
    pair_map = defaultdict(list)
    for pool in all_pools:
        t0, t1 = pool["token0"], pool["token1"]
        key = tuple(sorted([t0, t1]))
        pair_map[key].append(pool)

    arb_pairs = {}
    for key, pools in pair_map.items():
        dexes = set(p["dex"] for p in pools)
        if len(dexes) >= 2:
            arb_pairs[f"{key[0]}_{key[1]}"] = {
                "tokens": list(key),
                "n_pools": len(pools),
                "n_dexes": len(dexes),
                "dexes": list(dexes),
                "pools": pools,
            }

    with open(ARB_PAIRS_FILE, "w") as f:
        json.dump(arb_pairs, f, indent=2)

    print(f"Arb candidate pairs (2+ DEXes): {len(arb_pairs)}")
    print("\nTop arb pairs by pool count:")
    for pair_key, info in sorted(arb_pairs.items(), key=lambda x: -x[1]["n_pools"])[:15]:
        tokens = [KNOWN_TOKENS.get(t, t[:10]) for t in info["tokens"]]
        print(f"  {'/'.join(tokens)}: {info['n_pools']} pools on {info['dexes']}")


KNOWN_TOKENS = {
    "0x82af49447d8a07e3bd95bd0d56f35241523fbab1": "WETH",
    "0xaf88d065e77c8cc2239327c5edb3a432268e5831": "USDC",
    "0xff970a61a04b1ca14834a43f5de4533ebddb5cc8": "USDC.e",
    "0xfd086bc7cd5c481dcc9c85ebe478a1c0b69fcbb9": "USDT",
    "0x912ce59144191c1204e64559fe8253a0e49e6548": "ARB",
    "0x2f2a2543b76a4166549f7aab2e75bef0aefc5b0f": "WBTC",
    "0xfc5a1a6eb076a2c7ad06ed22c90d7e710e35ad0a": "GMX",
    "0xf97f4df75117a78c1a5a0dbb814af92458539fb4": "LINK",
    "0x539bde0d7dbd336b79148aa742883198bbf60342": "MAGIC",
}

if __name__ == "__main__":
    discover_all()
