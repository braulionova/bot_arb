// SPDX-License-Identifier: MIT
pragma solidity ^0.8.20;

import "forge-std/Script.sol";
import "../src/ArbExecutor.sol";

contract DeployScript is Script {
    function run() external {
        uint256 deployerPrivateKey = vm.envUint("PRIVATE_KEY");

        vm.startBroadcast(deployerPrivateKey);

        ArbExecutor arb = new ArbExecutor();
        console.log("ArbExecutor deployed at:", address(arb));
        console.log("Owner:", arb.owner());
        console.log("Balancer Vault:", address(arb.vault()));

        // With direct pool calls, no router approvals needed!
        // V3: tokens transferred inside uniswapV3SwapCallback (direct to pool)
        // V2: tokens transferred directly to pair before swap()
        // Only Balancer Vault needs approval for flash loan repayments

        address WETH  = 0x82aF49447D8a07e3bd95BD0d56f35241523fBab1;
        address USDC  = 0xaf88d065e77c8cC2239327C5EDb3A432268e5831;
        address USDCe = 0xFF970A61A04b1cA14834A43f5dE4533eBDDB5CC8;
        address USDT  = 0xFd086bC7CD5C481DCC9C85ebE478A1C0b69FCbb9;
        address ARB_TOKEN = 0x912CE59144191C1204E64559FE8253a0e49E6548;
        address WBTC  = 0x2f2a2543B76A4166549F7aaB2e75Bef0aefC5B0f;
        address GMX   = 0xfc5A1A6EB076a2C7aD06eD22C90d7E710E35ad0a;
        address DAI   = 0xDA10009cBd5D07dd0CeCc66161FC93D7c9000da1;

        address BALANCER_VAULT = 0xBA12222222228d8Ba445958a75a0704d566BF2C8;

        address AAVE_POOL = 0x794a61358D6845594F94dc1DB02A252b5b4814aD;

        address[8] memory tokens = [WETH, USDC, USDCe, USDT, ARB_TOKEN, WBTC, GMX, DAI];
        for (uint i = 0; i < tokens.length; i++) {
            arb.setApproval(tokens[i], BALANCER_VAULT, type(uint256).max);
            arb.setApproval(tokens[i], AAVE_POOL, type(uint256).max);
        }

        console.log("Balancer Vault + Aave Pool approved for all tokens");

        vm.stopBroadcast();
    }
}
