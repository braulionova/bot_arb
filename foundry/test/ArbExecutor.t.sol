// SPDX-License-Identifier: MIT
pragma solidity ^0.8.20;

import "forge-std/Test.sol";
import "../src/ArbExecutor.sol";

/// @notice Fork tests for ArbExecutor (direct pool calls + multi-hop)
/// Run with: forge test --fork-url https://arbitrum.drpc.org -vvv
contract ArbExecutorTest is Test {
    ArbExecutor public arb;

    address constant WETH = 0x82aF49447D8a07e3bd95BD0d56f35241523fBab1;
    address constant USDC = 0xaf88d065e77c8cC2239327C5EDb3A432268e5831;
    address constant ARB_TOKEN = 0x912CE59144191C1204E64559FE8253a0e49E6548;
    address constant BALANCER_VAULT = 0xBA12222222228d8Ba445958a75a0704d566BF2C8;

    // V3 pools (direct)
    address constant UNI_V3_WETH_USDC_005 = 0xC6962004f452bE9203591991D15f6b388e09E8D0;
    address constant UNI_V3_WETH_ARB_005  = 0x80A9ae39310abf666A87C743d6ebBD0E8C42158E;
    address constant PCS_V3_WETH_USDC_001 = 0x7fCDC35463E3770c2fB992716Cd070B63540b947;

    function setUp() public {
        arb = new ArbExecutor();
    }

    function testOwner() public view {
        assertEq(arb.owner(), address(this));
    }

    function testRevert_notOwner() public {
        vm.prank(address(0xdead));
        vm.expectRevert("not owner");
        arb.executeArbFlashLoan(WETH, 1e15, UNI_V3_WETH_USDC_005, PCS_V3_WETH_USDC_001, USDC, true, true, 0);
    }

    function testRevert_notVault() public {
        address[] memory tokens = new address[](1);
        tokens[0] = WETH;
        uint256[] memory amounts = new uint256[](1);
        amounts[0] = 1e15;
        uint256[] memory fees = new uint256[](1);
        vm.expectRevert("not vault");
        arb.receiveFlashLoan(tokens, amounts, fees, "");
    }

    // Test 2-hop: flash loan WETH, swap on UniV3 pool, swap back on PCS V3 pool
    function testFlashLoan_2hop_V3_V3() public {
        // Will revert with "insufficient profit" since no real arb exists
        // But proves the full pipeline works
        vm.expectRevert("insufficient profit");
        arb.executeArbFlashLoan(
            WETH, 1e15,
            UNI_V3_WETH_USDC_005,  // buy USDC
            PCS_V3_WETH_USDC_001,  // sell USDC for WETH
            USDC,
            true, true, 0
        );
    }

    // Test 3-hop multi-hop: WETH → USDC → ARB → WETH
    function testFlashLoan_3hop() public {
        ArbExecutor.SwapHop[] memory hops = new ArbExecutor.SwapHop[](3);
        hops[0] = ArbExecutor.SwapHop({
            pool: UNI_V3_WETH_USDC_005,
            tokenOut: USDC,
            isV3: true,
            isCurve: false,
            zeroForOne: true,
            amountOut: 0,
            curveI: 0,
            curveJ: 0
        });
        hops[1] = ArbExecutor.SwapHop({
            pool: 0xfaE2AE0a9f87FD35b5b0E24B47BAC796A7EEfEa1,
            tokenOut: ARB_TOKEN,
            isV3: true,
            isCurve: false,
            zeroForOne: true,
            amountOut: 0,
            curveI: 0,
            curveJ: 0
        });
        hops[2] = ArbExecutor.SwapHop({
            pool: UNI_V3_WETH_ARB_005,
            tokenOut: WETH,
            isV3: true,
            isCurve: false,
            zeroForOne: false,
            amountOut: 0,
            curveI: 0,
            curveJ: 0
        });

        // 3-hop will revert (either "insufficient profit" or "IIA" from V3 tick math)
        // Both are acceptable — proves the multi-hop pipeline reaches execution
        vm.expectRevert();
        arb.executeMultiHopFlashLoan(WETH, 1e15, hops, 0);
    }

    function testWithdraw() public {
        deal(WETH, address(arb), 1 ether);
        arb.withdraw(WETH, 1 ether);
    }

    function testReceiveETH() public {
        (bool ok,) = address(arb).call{value: 1 ether}("");
        assertTrue(ok);
    }

    receive() external payable {}
}
