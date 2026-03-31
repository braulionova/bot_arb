// SPDX-License-Identifier: MIT
pragma solidity ^0.8.20;

/// @title ArbExecutor - Atomic cross-DEX arbitrage with Balancer V2 flash loans
/// @notice Direct pool calls (no routers) — saves ~60-100k gas per 2-leg arb
/// @dev Top Arbitrum MEV bots (e.g. 0xF7F2d68B832E) use this pattern

interface IERC20 {
    function balanceOf(address account) external view returns (uint256);
    function transfer(address to, uint256 amount) external returns (bool);
    function approve(address spender, uint256 amount) external returns (bool);
}

// ─── Direct pool interfaces (no routers!) ───

interface IUniswapV2Pair {
    function swap(uint256 amount0Out, uint256 amount1Out, address to, bytes calldata data) external;
    function getReserves() external view returns (uint112 reserve0, uint112 reserve1, uint32 blockTimestampLast);
    function token0() external view returns (address);
}

/// @dev Camelot V2 has per-token directional fees
interface ICamelotPair {
    function token0FeePercent() external view returns (uint16);
    function token1FeePercent() external view returns (uint16);
    function stableSwap() external view returns (bool);
}

interface IUniswapV3Pool {
    function swap(
        address recipient,
        bool zeroForOne,
        int256 amountSpecified,
        uint160 sqrtPriceLimitX96,
        bytes calldata data
    ) external returns (int256 amount0, int256 amount1);
    function token0() external view returns (address);
    function token1() external view returns (address);
}

interface ICurvePool {
    function exchange(int128 i, int128 j, uint256 dx, uint256 min_dy) external returns (uint256);
    function coins(uint256 i) external view returns (address);
}

// Balancer V2 Vault interface
interface IBalancerVault {
    function flashLoan(
        address recipient,
        address[] memory tokens,
        uint256[] memory amounts,
        bytes memory userData
    ) external;
}

interface IFlashLoanRecipient {
    function receiveFlashLoan(
        address[] memory tokens,
        uint256[] memory amounts,
        uint256[] memory feeAmounts,
        bytes memory userData
    ) external;
}

// Aave V3 flash loan + liquidation interfaces
interface IAavePool {
    function flashLoanSimple(
        address receiverAddress,
        address asset,
        uint256 amount,
        bytes calldata params,
        uint16 referralCode
    ) external;

    function liquidationCall(
        address collateralAsset,
        address debtAsset,
        address user,
        uint256 debtToCover,
        bool receiveAToken
    ) external;
}

interface IAaveFlashLoanReceiver {
    function executeOperation(
        address asset,
        uint256 amount,
        uint256 premium,
        address initiator,
        bytes calldata params
    ) external returns (bool);
}

contract ArbExecutor is IFlashLoanRecipient, IAaveFlashLoanReceiver {
    address public immutable owner;
    IBalancerVault public constant vault = IBalancerVault(0xBA12222222228d8Ba445958a75a0704d566BF2C8);
    IAavePool public constant aavePool = IAavePool(0x794a61358D6845594F94dc1DB02A252b5b4814aD);

    /// V3 sqrt price limits (used when no specific limit desired)
    uint160 internal constant MIN_SQRT_RATIO = 4295128740;   // MIN + 1
    uint160 internal constant MAX_SQRT_RATIO = 1461446703485210103287273052203988822378723970341; // MAX - 1

    /// @dev Reentrancy guard for V3 callbacks
    address private _expectedCallbackPool;

    /// @dev Temporary storage for multi-hop operations (set before flashLoan, cleared after)
    SwapHop[] private _pendingHops;
    uint256 private _pendingMinProfit;

    // ─── Liquidation storage ───

    struct LiquidationParams {
        address lendingPool;
        address borrower;
        address collateralToken;
        address debtToken;
        address sellPool;
        bool sellIsV3;
        bool sellIsCurve;
        uint256 minProfit;
    }

    /// @dev Pending liquidation set before flash loan callback; cleared in callback
    LiquidationParams private _pendingLiquidation;

    modifier onlyOwner() {
        require(msg.sender == owner, "not owner");
        _;
    }

    modifier onlyVault() {
        require(msg.sender == address(vault), "not vault");
        _;
    }

    modifier onlyAave() {
        require(msg.sender == address(aavePool), "not aave");
        _;
    }

    constructor() {
        owner = msg.sender;
    }

    // ═══════════════════════════════════════════════════════════════
    //  ENTRY POINTS
    // ═══════════════════════════════════════════════════════════════

    /// @notice Pre-approve tokens for direct transfers (not needed for V3 callbacks)
    function setApproval(address token, address spender, uint256 amount) external onlyOwner {
        IERC20(token).approve(spender, amount);
    }

    /// @notice Execute arb via Balancer V2 flash loan (0% fee)
    /// @param tokenIn The token to borrow and end up with profit in
    /// @param amountIn Amount to flash loan
    /// @param buyPool Pool address to buy tokenOut (lower price)
    /// @param sellPool Pool address to sell tokenOut (higher price)
    /// @param tokenOut Intermediate token
    /// @param buyIsV3 True if buyPool is a V3 pool
    /// @param sellIsV3 True if sellPool is a V3 pool
    /// @param minProfit Minimum profit in tokenIn terms (0 = any profit)
    function executeArbFlashLoan(
        address tokenIn,
        uint256 amountIn,
        address buyPool,
        address sellPool,
        address tokenOut,
        bool buyIsV3,
        bool sellIsV3,
        uint256 minProfit
    ) external onlyOwner {
        bytes memory userData = abi.encode(
            buyPool, sellPool, tokenOut, buyIsV3, sellIsV3, minProfit
        );

        address[] memory tokens = new address[](1);
        tokens[0] = tokenIn;
        uint256[] memory amounts = new uint256[](1);
        amounts[0] = amountIn;

        vault.flashLoan(address(this), tokens, amounts, userData);
    }

    /// @notice Balancer V2 flash loan callback
    function receiveFlashLoan(
        address[] memory tokens,
        uint256[] memory amounts,
        uint256[] memory feeAmounts,
        bytes memory userData
    ) external onlyVault {
        address tokenIn = tokens[0];
        uint256 amount = amounts[0];
        uint256 fee = feeAmounts[0];
        uint256 totalDebt = amount + fee;

        // ── Liquidation branch ──
        if (_pendingLiquidation.lendingPool != address(0)) {
            LiquidationParams memory liq = _pendingLiquidation;
            delete _pendingLiquidation;

            uint256 balanceBefore = IERC20(liq.debtToken).balanceOf(address(this));

            // 1. Approve lending pool to pull the debt tokens
            IERC20(liq.debtToken).approve(liq.lendingPool, amount);

            // 2. Execute liquidation — we receive collateralToken at a discount
            IAavePool(liq.lendingPool).liquidationCall(
                liq.collateralToken,
                liq.debtToken,
                liq.borrower,
                amount,
                false // receive underlying collateral, not aToken
            );

            // 3. Sell received collateral back to debtToken on the specified DEX
            uint256 collateralReceived = IERC20(liq.collateralToken).balanceOf(address(this));
            require(collateralReceived > 0, "no collateral received");

            if (liq.sellIsCurve) {
                _swapCurve(liq.sellPool, liq.collateralToken, liq.debtToken, collateralReceived);
            } else if (liq.sellIsV3) {
                _swapV3(liq.sellPool, liq.collateralToken, liq.debtToken, collateralReceived);
            } else {
                _swapV2(liq.sellPool, liq.collateralToken, liq.debtToken, collateralReceived, address(this));
            }

            // 4. Verify profitability
            uint256 balanceAfter = IERC20(liq.debtToken).balanceOf(address(this));
            require(
                balanceAfter >= balanceBefore + totalDebt + liq.minProfit,
                "liquidation unprofitable"
            );

            // 5. Repay Balancer flash loan (Balancer V2 fee = 0%)
            IERC20(liq.debtToken).transfer(address(vault), totalDebt);
            return;
        }

        // ── Arb branch (existing logic) ──

        uint256 balanceBefore = IERC20(tokenIn).balanceOf(address(this));

        uint256 minProfit;
        if (_pendingHops.length > 0) {
            // Multi-hop path (hops stored in _pendingHops)
            minProfit = _pendingMinProfit;
            _doMultiHop(tokenIn, amount, _pendingHops);
            // Clear storage
            delete _pendingHops;
            _pendingMinProfit = 0;
        } else {
            // 2-hop path
            (
                address buyPool,
                address sellPool,
                address tokenOut,
                bool buyIsV3,
                bool sellIsV3,
                uint256 mp
            ) = abi.decode(userData, (address, address, address, bool, bool, uint256));
            minProfit = mp;
            _doArb(tokenIn, amount, buyPool, sellPool, tokenOut, buyIsV3, sellIsV3);
        }

        uint256 balanceAfter = IERC20(tokenIn).balanceOf(address(this));
        require(balanceAfter >= balanceBefore + totalDebt + minProfit, "insufficient profit");

        IERC20(tokenIn).transfer(address(vault), totalDebt);
    }

    /// @notice Execute arb without flash loan (using contract balance)
    function executeArb(
        address tokenIn,
        uint256 amountIn,
        address buyPool,
        address sellPool,
        address tokenOut,
        bool buyIsV3,
        bool sellIsV3,
        uint256 minProfit
    ) external onlyOwner {
        uint256 balanceBefore = IERC20(tokenIn).balanceOf(address(this));
        require(balanceBefore >= amountIn, "insufficient balance");

        _doArb(tokenIn, amountIn, buyPool, sellPool, tokenOut, buyIsV3, sellIsV3);

        uint256 balanceAfter = IERC20(tokenIn).balanceOf(address(this));
        require(balanceAfter >= balanceBefore + minProfit, "insufficient profit");
    }

    // ═══════════════════════════════════════════════════════════════
    //  MULTI-HOP ARB — 3+ leg routes (e.g. WETH→ARB→USDC→WETH)
    // ═══════════════════════════════════════════════════════════════

    struct SwapHop {
        address pool;       // Direct pool address
        address tokenOut;   // Output token of this leg
        bool isV3;          // True = V3 pool, False = V2 pool
        bool isCurve;       // True = Curve/Balancer stable pool
        bool zeroForOne;    // Pre-computed: tokenIn < tokenOut (swap direction)
        uint256 amountOut;  // Pre-computed V2 amountOut (0 = compute on-chain for V3/Curve)
        int128 curveI;      // Pre-computed Curve coin index i
        int128 curveJ;      // Pre-computed Curve coin index j
    }

    /// @notice Execute multi-hop arb via Balancer flash loan
    /// @param flashToken Token to borrow (start and end of cycle)
    /// @param flashAmount Amount to borrow
    /// @param hops Array of swap legs — last hop must output flashToken
    /// @param minProfit Minimum profit in flashToken terms
    function executeMultiHopFlashLoan(
        address flashToken,
        uint256 flashAmount,
        SwapHop[] calldata hops,
        uint256 minProfit
    ) external onlyOwner {
        // Store hops in storage (cleared after callback)
        for (uint i = 0; i < hops.length; i++) {
            _pendingHops.push(hops[i]);
        }
        _pendingMinProfit = minProfit;

        // userData = empty signals multi-hop (hops are in storage)
        bytes memory userData = "";

        address[] memory tokens = new address[](1);
        tokens[0] = flashToken;
        uint256[] memory amounts = new uint256[](1);
        amounts[0] = flashAmount;

        vault.flashLoan(address(this), tokens, amounts, userData);
    }

    // ─── Aave V3 Flash Loan Entry Points ───

    /// @notice 2-hop arb via Aave V3 flash loan (fallback when Balancer lacks the token)
    function executeArbAaveFL(
        address tokenIn,
        uint256 amountIn,
        address buyPool,
        address sellPool,
        address tokenOut,
        bool buyIsV3,
        bool sellIsV3,
        uint256 minProfit
    ) external onlyOwner {
        bytes memory params = abi.encode(buyPool, sellPool, tokenOut, buyIsV3, sellIsV3, minProfit);
        aavePool.flashLoanSimple(address(this), tokenIn, amountIn, params, 0);
    }

    /// @notice Multi-hop arb via Aave V3 flash loan
    function executeMultiHopAaveFL(
        address flashToken,
        uint256 flashAmount,
        SwapHop[] calldata hops,
        uint256 minProfit
    ) external onlyOwner {
        for (uint i = 0; i < hops.length; i++) {
            _pendingHops.push(hops[i]);
        }
        _pendingMinProfit = minProfit;
        aavePool.flashLoanSimple(address(this), flashToken, flashAmount, "", 0);
    }

    // ─── Liquidation Entry Point ───

    /// @notice Execute liquidation via Balancer flash loan (0% fee)
    /// Flow: flash loan debtToken → liquidate on Aave → receive discounted collateral
    ///       → sell collateral on DEX → repay flash loan → keep profit
    /// @param lendingPool  Aave V3 (or fork) pool address
    /// @param borrower     Under-collateralised position to liquidate
    /// @param debtToken    Token the borrower owes (we flash-loan this)
    /// @param debtAmount   Amount of debt to cover (≤ 50% of total debt, Aave close factor)
    /// @param collateralToken  Collateral we receive after liquidation
    /// @param sellPool     DEX pool used to swap received collateral back to debtToken
    /// @param sellIsV3     True if sellPool is a UniswapV3-style pool
    /// @param sellIsCurve  True if sellPool is a Curve/Balancer stable pool
    /// @param minProfit    Minimum acceptable profit in debtToken units (reverts if not met)
    function liquidateFlashLoan(
        address lendingPool,
        address borrower,
        address debtToken,
        uint256 debtAmount,
        address collateralToken,
        address sellPool,
        bool sellIsV3,
        bool sellIsCurve,
        uint256 minProfit
    ) external onlyOwner {
        // Store liquidation params for the Balancer callback
        _pendingLiquidation = LiquidationParams({
            lendingPool: lendingPool,
            borrower: borrower,
            collateralToken: collateralToken,
            debtToken: debtToken,
            sellPool: sellPool,
            sellIsV3: sellIsV3,
            sellIsCurve: sellIsCurve,
            minProfit: minProfit
        });

        address[] memory tokens = new address[](1);
        tokens[0] = debtToken;
        uint256[] memory amounts = new uint256[](1);
        amounts[0] = debtAmount;

        vault.flashLoan(address(this), tokens, amounts, abi.encode("LIQUIDATE"));
    }

    /// @notice Aave V3 flash loan callback
    function executeOperation(
        address asset,
        uint256 amount,
        uint256 premium,
        address initiator,
        bytes calldata params
    ) external onlyAave returns (bool) {
        require(initiator == address(this), "bad initiator");

        uint256 balanceBefore = IERC20(asset).balanceOf(address(this));

        uint256 minProfit;
        if (_pendingHops.length > 0) {
            minProfit = _pendingMinProfit;
            _doMultiHop(asset, amount, _pendingHops);
            delete _pendingHops;
            _pendingMinProfit = 0;
        } else {
            (
                address buyPool, address sellPool, address tokenOut,
                bool buyIsV3, bool sellIsV3, uint256 mp
            ) = abi.decode(params, (address, address, address, bool, bool, uint256));
            minProfit = mp;
            _doArb(asset, amount, buyPool, sellPool, tokenOut, buyIsV3, sellIsV3);
        }

        uint256 totalDebt = amount + premium;
        uint256 balanceAfter = IERC20(asset).balanceOf(address(this));
        require(balanceAfter >= balanceBefore + totalDebt + minProfit, "insufficient profit");

        // Approve Aave to pull back debt
        IERC20(asset).approve(address(aavePool), totalDebt);
        return true;
    }

    // ═══════════════════════════════════════════════════════════════
    //  CORE ARB LOGIC — DIRECT POOL CALLS
    // ═══════════════════════════════════════════════════════════════

    /// @dev Execute N-hop swap cycle: token → hop[0].tokenOut → hop[1].tokenOut → ... → token
    function _doMultiHop(
        address startToken,
        uint256 startAmount,
        SwapHop[] memory hops
    ) internal {
        address currentToken = startToken;
        uint256 currentAmount = startAmount;

        for (uint i = 0; i < hops.length; i++) {
            // For intermediate hops, use full balance minus a small buffer for V3 fee rounding
            if (i > 0) {
                uint256 bal = IERC20(currentToken).balanceOf(address(this));
                // Reduce by 1% to account for V3 tick-crossing fee rounding
                currentAmount = bal * 99 / 100;
            }
            if (hops[i].isCurve) {
                currentAmount = _swapCurveFast(hops[i].pool, currentToken, currentAmount, hops[i].curveI, hops[i].curveJ);
            } else if (hops[i].isV3) {
                currentAmount = _swapV3Fast(hops[i].pool, currentToken, currentAmount, hops[i].zeroForOne);
            } else {
                currentAmount = _swapV2Fast(hops[i].pool, currentToken, currentAmount, hops[i].zeroForOne, hops[i].amountOut, address(this));
            }
            currentToken = hops[i].tokenOut;
        }
    }

    /// @dev Buy tokenOut on buyPool, sell back for tokenIn on sellPool
    ///      Uses pool-specific fee for V2 swaps (supports Camelot variable fees)
    function _doArb(
        address tokenIn,
        uint256 amountIn,
        address buyPool,
        address sellPool,
        address tokenOut,
        bool buyIsV3,
        bool sellIsV3
    ) internal {
        uint256 amountOut;

        // ── Leg 1: Buy tokenOut ──
        if (buyIsV3) {
            amountOut = _swapV3(buyPool, tokenIn, tokenOut, amountIn);
        } else {
            amountOut = _swapV2(buyPool, tokenIn, tokenOut, amountIn, address(this));
        }

        // ── Leg 2: Sell tokenOut for tokenIn ──
        if (sellIsV3) {
            _swapV3(sellPool, tokenOut, tokenIn, amountOut);
        } else {
            _swapV2(sellPool, tokenOut, tokenIn, amountOut, address(this));
        }
    }

    // ═══════════════════════════════════════════════════════════════
    //  FAST SWAP FUNCTIONS — zero on-chain lookups, all pre-computed
    // ═══════════════════════════════════════════════════════════════

    /// @dev V2 swap with pre-computed direction and amountOut — saves ~7.5k gas
    ///      No getReserves() or token0() calls needed
    function _swapV2Fast(
        address pair,
        address tokenIn,
        uint256 amountIn,
        bool zeroForOne,
        uint256 precomputedAmountOut,
        address recipient
    ) internal returns (uint256 amountOut) {
        IERC20(tokenIn).transfer(pair, amountIn);

        if (precomputedAmountOut > 0) {
            // Use pre-computed amountOut from off-chain simulation
            amountOut = precomputedAmountOut;
        } else {
            // Fallback: compute on-chain with dynamic fee detection
            (uint112 reserve0, uint112 reserve1, ) = IUniswapV2Pair(pair).getReserves();
            (uint256 reserveIn, uint256 reserveOut) = zeroForOne
                ? (uint256(reserve0), uint256(reserve1))
                : (uint256(reserve1), uint256(reserve0));

            // Detect Camelot-style directional fee
            (bool ok, bytes memory feeData) = pair.staticcall(
                abi.encodeWithSelector(
                    zeroForOne
                        ? ICamelotPair.token0FeePercent.selector
                        : ICamelotPair.token1FeePercent.selector
                )
            );
            if (ok && feeData.length >= 32) {
                uint256 fee = abi.decode(feeData, (uint256));
                if (fee > 0 && fee < 100000) {
                    uint256 amountInWithFee = amountIn * (100000 - fee);
                    amountOut = (amountInWithFee * reserveOut) / (reserveIn * 100000 + amountInWithFee);
                } else {
                    uint256 amountInWithFee = amountIn * 997;
                    amountOut = (amountInWithFee * reserveOut) / (reserveIn * 1000 + amountInWithFee);
                }
            } else {
                uint256 amountInWithFee = amountIn * 997;
                amountOut = (amountInWithFee * reserveOut) / (reserveIn * 1000 + amountInWithFee);
            }
        }

        if (zeroForOne) {
            IUniswapV2Pair(pair).swap(0, amountOut, recipient, "");
        } else {
            IUniswapV2Pair(pair).swap(amountOut, 0, recipient, "");
        }
    }

    /// @dev V3 swap with pre-computed direction — saves ~2.6k gas (no token0() call)
    function _swapV3Fast(
        address pool,
        address tokenIn,
        uint256 amountIn,
        bool zeroForOne
    ) internal returns (uint256 amountOut) {
        _expectedCallbackPool = pool;

        (int256 amount0, int256 amount1) = IUniswapV3Pool(pool).swap(
            address(this),
            zeroForOne,
            int256(amountIn),
            zeroForOne ? MIN_SQRT_RATIO : MAX_SQRT_RATIO,
            abi.encode(tokenIn)
        );

        amountOut = zeroForOne ? uint256(-amount1) : uint256(-amount0);
    }

    /// @dev Curve swap with pre-computed indices — saves ~2.6k gas (no coins() call)
    function _swapCurveFast(
        address pool,
        address tokenIn,
        uint256 amountIn,
        int128 i,
        int128 j
    ) internal returns (uint256 amountOut) {
        IERC20(tokenIn).approve(pool, amountIn);
        amountOut = ICurvePool(pool).exchange(i, j, amountIn, 0);
    }

    // ═══════════════════════════════════════════════════════════════
    //  CURVE STABLE SWAP — approve-then-exchange pattern (legacy)
    // ═══════════════════════════════════════════════════════════════

    /// @dev Direct Curve pool exchange. Uses approve + exchange() pattern.
    ///      Curve stable pools require ERC-20 approval (unlike V2 push-and-swap).
    ///      index i/j determined by calling coins() — costs ~2 SLOADs but unavoidable.
    function _swapCurve(
        address pool,
        address tokenIn,
        address /* tokenOut */,
        uint256 amountIn
    ) internal returns (uint256 amountOut) {
        // Determine coin indices
        address coin0 = ICurvePool(pool).coins(0);
        int128 i = (tokenIn == coin0) ? int128(0) : int128(1);
        int128 j = (i == 0) ? int128(1) : int128(0);

        // Approve pool to pull tokenIn
        IERC20(tokenIn).approve(pool, amountIn);

        // min_dy = 0: accept any output (we verify profit at the end)
        amountOut = ICurvePool(pool).exchange(i, j, amountIn, 0);
    }

    // ═══════════════════════════════════════════════════════════════
    //  V2 DIRECT SWAP — transfer-then-swap pattern
    // ═══════════════════════════════════════════════════════════════

    /// @dev Direct V2 pair swap. Transfers tokenIn to pair, then calls swap().
    ///      Supports variable fees (Camelot V2 directional fees, SushiSwap, etc.)
    ///      Detects fee by trying to call token0FeePercent() — if it exists,
    ///      uses the directional fee; otherwise defaults to 0.3% (997/1000).
    function _swapV2(
        address pair,
        address tokenIn,
        address /* tokenOut */,
        uint256 amountIn,
        address recipient
    ) internal returns (uint256 amountOut) {
        // Transfer tokens directly to the pair (cheaper than approve+transferFrom)
        IERC20(tokenIn).transfer(pair, amountIn);

        // Read reserves and determine direction
        (uint112 reserve0, uint112 reserve1, ) = IUniswapV2Pair(pair).getReserves();
        address token0 = IUniswapV2Pair(pair).token0();

        bool isToken0In = (tokenIn == token0);
        (uint256 reserveIn, uint256 reserveOut) = isToken0In
            ? (uint256(reserve0), uint256(reserve1))
            : (uint256(reserve1), uint256(reserve0));

        // Detect fee: try reading Camelot-style directional fee
        // token0FeePercent/token1FeePercent are in basis points (e.g. 300 = 0.3%)
        // Falls back to standard 30 bps (0.3%) if call fails
        uint256 feeBps = 30; // default 0.3%
        (bool ok, bytes memory feeData) = pair.staticcall(
            abi.encodeWithSelector(
                isToken0In
                    ? ICamelotPair.token0FeePercent.selector
                    : ICamelotPair.token1FeePercent.selector
            )
        );
        if (ok && feeData.length >= 32) {
            uint256 rawFee = abi.decode(feeData, (uint256));
            if (rawFee > 0 && rawFee < 10000) {
                feeBps = rawFee / 100; // Camelot returns e.g. 300 for 0.3%, we need 30 bps
                // Camelot fee is already in basis points of 100 (e.g. 300 = 3%)
                // Actually Camelot uses "fee percent" * 100, e.g. token0FeePercent() = 300 means 0.3%
                // So feeBps = rawFee / 100 = 3, then feeMultiplier = 10000 - 3*100 = 9700? No.
                // Let's be safe: Camelot token0FeePercent() returns values like 150, 200, 300
                // where 300 = 0.3%. So rawFee is already in hundredths of a percent.
                // feeMultiplier = (10000 - rawFee) for the AMM formula
                // Use rawFee directly as basis-points-of-100
                feeBps = rawFee; // e.g. 300
            }
        }

        // AMM formula with dynamic fee
        // feeBps is in "Camelot units" (300 = 0.3%) or default 30
        // Normalize: for standard V2, feeBps=30 → multiplier=997 (30 bps = 0.3%)
        // For Camelot, feeBps=300 → multiplier=997 (300/10 = 30 bps = 0.3%)
        // Camelot returns fee as percent*100: 150=0.15%, 200=0.2%, 300=0.3%
        uint256 feeMultiplier;
        if (ok && feeData.length >= 32) {
            // Camelot fee: rawFee is in hundredths of percent (300 = 0.3%)
            // feeMultiplier = 10000 - rawFee (e.g. 10000-300=9700, then /10 = 970... no)
            // Actually: Camelot pair uses fee like: amountIn * (10000-fee) / 10000
            // where fee = token0FeePercent = e.g. 300 means 3% of 10000 basis
            // Wait, let me re-check. Camelot V2 pairs:
            // FEE_DENOMINATOR = 100000, default fee = 300 (0.3%)
            // amountInWithFee = amountIn * (FEE_DENOMINATOR - fee) / FEE_DENOMINATOR
            // So: 100000 - 300 = 99700, then * reserveOut / (reserveIn * 100000 + amountIn * 99700)
            // This is slightly different from UniV2 (which uses 1000 denominator)
            feeMultiplier = 100000 - feeBps; // e.g. 100000 - 300 = 99700
            uint256 amountInWithFee = amountIn * feeMultiplier;
            amountOut = (amountInWithFee * reserveOut) / (reserveIn * 100000 + amountInWithFee);
        } else {
            // Standard UniV2: 997/1000 = 0.3% fee
            uint256 amountInWithFee = amountIn * 997;
            amountOut = (amountInWithFee * reserveOut) / (reserveIn * 1000 + amountInWithFee);
        }

        if (isToken0In) {
            IUniswapV2Pair(pair).swap(0, amountOut, recipient, "");
        } else {
            IUniswapV2Pair(pair).swap(amountOut, 0, recipient, "");
        }
    }

    // ═══════════════════════════════════════════════════════════════
    //  V3 DIRECT SWAP — call pool.swap() with callback
    // ═══════════════════════════════════════════════════════════════

    /// @dev Direct V3 pool swap. Calls pool.swap() which triggers a callback.
    ///      Saves ~30-50k gas vs router (no struct decoding, no transferFrom,
    ///      no extra external call hop).
    function _swapV3(
        address pool,
        address tokenIn,
        address tokenOut,
        uint256 amountIn
    ) internal returns (uint256 amountOut) {
        // Determine swap direction
        address token0 = IUniswapV3Pool(pool).token0();
        bool zeroForOne = (tokenIn == token0);

        // Set callback expectation for security
        _expectedCallbackPool = pool;

        // Call pool.swap() directly
        // amountSpecified > 0 = exact input
        (int256 amount0, int256 amount1) = IUniswapV3Pool(pool).swap(
            address(this),          // recipient
            zeroForOne,
            int256(amountIn),       // exact input amount
            zeroForOne ? MIN_SQRT_RATIO : MAX_SQRT_RATIO,  // no price limit
            abi.encode(tokenIn)     // callback data: which token to pay
        );

        // Output amount is the negative delta (tokens sent to us)
        amountOut = zeroForOne ? uint256(-amount1) : uint256(-amount0);
    }

    // ═══════════════════════════════════════════════════════════════
    //  V3 CALLBACKS — one for each DEX fork
    // ═══════════════════════════════════════════════════════════════

    /// @dev Shared callback logic. Pool calls this after computing swap amounts.
    ///      We MUST transfer the owed tokens to the pool here or it reverts.
    function _v3SwapCallback(
        int256 amount0Delta,
        int256 amount1Delta,
        bytes calldata data
    ) internal {
        // Security: only the expected pool can call this
        require(msg.sender == _expectedCallbackPool, "unauthorized callback");
        _expectedCallbackPool = address(0);

        // One delta is positive (amount we owe), one is negative (amount we received)
        address tokenIn = abi.decode(data, (address));
        uint256 amountOwed = amount0Delta > 0 ? uint256(amount0Delta) : uint256(amount1Delta);

        // Transfer exactly what's owed — never more (avoid donating to pool)
        IERC20(tokenIn).transfer(msg.sender, amountOwed);
    }

    /// @notice Uniswap V3 / SushiSwap V3 callback
    function uniswapV3SwapCallback(
        int256 amount0Delta,
        int256 amount1Delta,
        bytes calldata data
    ) external {
        _v3SwapCallback(amount0Delta, amount1Delta, data);
    }

    /// @notice PancakeSwap V3 callback
    function pancakeV3SwapCallback(
        int256 amount0Delta,
        int256 amount1Delta,
        bytes calldata data
    ) external {
        _v3SwapCallback(amount0Delta, amount1Delta, data);
    }

    /// @notice Camelot V3 / Algebra callback
    function algebraSwapCallback(
        int256 amount0Delta,
        int256 amount1Delta,
        bytes calldata data
    ) external {
        _v3SwapCallback(amount0Delta, amount1Delta, data);
    }

    // ═══════════════════════════════════════════════════════════════
    //  ADMIN
    // ═══════════════════════════════════════════════════════════════

    function withdraw(address token, uint256 amount) external onlyOwner {
        IERC20(token).transfer(owner, amount);
    }

    function withdrawETH() external onlyOwner {
        (bool success, ) = owner.call{value: address(this).balance}("");
        require(success, "ETH transfer failed");
    }

    receive() external payable {}
}
