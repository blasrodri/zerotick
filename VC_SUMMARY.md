# ZeroTick — Executive Summary

> **Status update (post-review fixes):** real NEP-141 custody is now wired
> end-to-end and proven on a local NEAR sandbox (deposit → trade → settle →
> claim, with refunds on rejected oracle bundles); new markets start at 0%
> vault allocation until the operator enables risk; sell payouts are capped
> at each position's reserved maximum so vault coverage survives any spot
> move; binary settlement requires an exact 0/1 fixing inside a 24h window.
> Still true: **not audited, not on mainnet, single oracle signer per
> market, no resolved data licensing or market/legal classification.**
> Treat everything below as the product thesis, not a deployment claim.

## The one-liner

**ZeroTick turns any trusted price into a tradable market.**

It is a shared execution and risk layer that lets market makers launch synthetic spot markets for assets that do not trade on-chain—emerging-market currencies, local equity indexes, stocks, commodities and eventually event outcomes—without tokenizing the underlying asset or seeding a conventional liquidity pool for every pair.

## The problem

There are prices for thousands of economically important assets, but most of them are effectively inaccessible to global internet-native traders.

A person outside Argentina who wants exposure to Argentine equities may need a local broker, local custody, currency conversion and access to a local exchange. Similar barriers exist across emerging-market FX, local indexes, single-name equities, rates and niche commodities.

Crypto markets have not solved this. A conventional automated market maker (AMM) needs an on-chain version of the asset and a funded token pool. If the asset has not been tokenized—or there is not enough native liquidity to support a pool—the market simply does not exist. An order-book exchange has the opposite problem: someone must continuously post and update quotes, manage inventory and remain active even when nobody is trading.

As a result, on-chain markets remain concentrated in a relatively small set of crypto-native assets while most of global finance remains outside that system.

## The ZeroTick solution

ZeroTick starts from a different question:

> If we already have a trustworthy external price, what else do we need to create a market?

The answer is three things:

1. A defined payoff.
2. Collateral sufficient to honor that payoff.
3. A pricing and risk policy that can be verified when a trade occurs.

ZeroTick packages those into a reusable market factory.

A market maker deposits USDC into a shared vault and registers a pricing formula for a market. An oracle service signs fresh market data—initially including the spot price, timestamp, sequence number and a recent-volatility measure. When a trader wants to buy or sell, the smart contract verifies the oracle signature and freshness, runs the market's formula and settles the trade against the vault.

There is no continuously updated on-chain order book and no token pool for the underlying asset.

## How a trade works

Suppose a trader wants exposure to USD/ARS or Argentina's Merval index:

1. The trader selects the market and enters an amount in USDC.
2. The interface receives a freshly signed oracle bundle containing the reference price, timestamp, recent price deviation and sequence number.
3. The contract verifies that the bundle was signed by the market's registered oracle, is not stale and is not a replay.
4. The formula calculates an executable bid and ask using the live reference price, a base spread, current vault inventory, recency of the data and recent volatility.
5. If the price is within the trader's limit and the vault has sufficient collateral and market-level risk capacity, the contract records the trader's synthetic spot position inside the protocol.
6. The trader can later sell the position back to the vault at the then-current formula bid and receive USDC.

In the current version, positions are internal to ZeroTick and are fully funded at 1×. There is no leverage, funding payment or liquidation mechanism. We do not issue an external token in this version.

## Why this is a PropAMM innovation

A PropAMM—short for proprietary automated market maker—uses a professional market maker's models, external prices and inventory management to generate prices algorithmically rather than relying only on a passive liquidity curve. PropAMMs can provide tighter and more responsive pricing, but today they are generally custom deployments built around assets that already trade on-chain.

ZeroTick combines the PropAMM model with a standardized market-creation protocol:

- Market-making logic is expressed as an auditable **executable formula**, rather than a stream of individual orders.
- The formula is evaluated only when a transaction occurs, so an inactive market does not require continuous on-chain quote updates.
- Pricing, inventory skew, collateral, settlement and oracle verification are shared infrastructure.
- The reference asset does not need to exist on-chain.

We think of ZeroTick as **PropAMM infrastructure for synthetic markets**: market makers bring capital and pricing expertise; ZeroTick provides the market factory, collateral and risk engine, oracle verification and settlement system.

## Core design choices

### Executable formulas, not orders

Each market has exactly one formula, selected from an audited registry and configured at creation. The formula defines valuation, spread, inventory adjustments, data-freshness penalties, volatility adjustments, exposure limits and settlement behavior. It is stored on-chain and can be inspected before trading.

This makes market creation closer to configuring software than building a new exchange.

### Time is explicitly priced

Most market systems treat a price as simply valid or invalid. ZeroTick treats freshness as part of the price. A one-second-old oracle observation receives the best spread; as the observation ages, the spread automatically widens. Beyond a hard freshness threshold, execution is rejected. Higher recent volatility also widens pricing and can trigger a circuit breaker.

### One shared, risk-managed vault

Liquidity providers and market makers fund one USDC vault that supports many markets. Each position reserves collateral against its market's stress limit, while per-market caps prevent a single market from consuming the whole vault.

### Synthetic spot first

The initial product avoids the complexity of perpetual futures. Positions are fully funded, have no funding rate and cannot be liquidated. This gives users economic exposure while keeping the initial risk model understandable.

## Why it matters

### It expands what can become a market

Any reliably priced reference can potentially become tradable: emerging-market FX, equity indexes, single-name stocks, ETF levels, commodities, rates and—using a binary formula—sports and prediction events. Debt requires additional treatment for coupons and accrued interest and is a later category.

### It reduces the cost of launching long-tail markets

A new market does not need a tokenized underlying, a listings process around custody, or a dedicated AMM pool. A creator selects an oracle, payoff and formula; the current prototype uses a flat $10 market-creation fee paid into vault collateral.

### It uses market-maker capital more efficiently

Capital does not sit separately in hundreds of token pairs. It sits in one diversified vault, while market-level controls and inventory-sensitive pricing help balance flow.

### It can make inaccessible exposure globally accessible

A user with USDC and an internet connection could gain exposure to markets that might otherwise require local financial infrastructure, subject to applicable regulation and product availability.

## What has been built

ZeroTick currently has a working research prototype:

- A Rust protocol core implementing the shared vault, signed oracle bundles, formula pricing, position accounting and settlement.
- Formula types for synthetic spot, capped index exposure, direct reference pricing and binary outcomes.
- A NEAR smart contract deployed and exercised on a local NEAR sandbox, including market creation, oracle verification, buys, sells and binary redemption.
- Negative-path checks for invalid signatures, stale data, replayed oracle sequences and excessive volatility.
- A trading terminal presenting market discovery, spot-denominated charts, oracle freshness, spread composition, vault coverage and position management.
- Local oracle tooling and public-market data experiments, including mapping Polymarket and Kalshi binary prices—which are both probabilities and prices for a $1 payout—into the same oracle-bundle format.

The web terminal's 62-market universe is currently a simulation of the product experience; the local-chain contract flow has been proven separately. ZeroTick is not yet audited or live on mainnet.

## Business model

Potential revenue sources include:

- A share of trading spread or explicit trading fees.
- Market-creation fees.
- Professional market-maker and oracle tooling.
- Premium APIs, analytics and institutional market operations.
- Longer-term vault or market-factory services for third-party venues.

## Why NEAR

NEAR provides low-cost computation, fast finality and an account model suitable for consumer-facing trading applications. Its intents and account-abstraction ecosystem may also simplify getting USDC into the vault despite markets referring to assets and data sources elsewhere.

## Key risks and work ahead

The core challenges are not primarily about drawing a trading interface:

- **Oracle quality, rights and licensing:** reliable data, signers and dispute processes are essential.
- **Market-maker risk:** vault participants need tools to hedge or otherwise manage exposure to markets whose underlying may trade on another venue or not at all.
- **Regulation:** synthetic exposure can be supervised differently according to asset class and jurisdiction.
- **Liquidity formation:** creating a market technically is easier than attracting balanced flow and committed capital.
- **Security:** the shared vault and settlement logic require extensive auditing and progressive limits.

Our roadmap therefore emphasizes oracle partnerships and data licenses, market-maker design relationships, expanded formula types, independent audits, real collateral-token integration and a limited curated launch before fully open market creation.

## Vision

Tokenization will bring many assets on-chain, but it will not bring every economically relevant price or outcome on-chain. ZeroTick is the execution layer for the rest: **if there is a trusted reference price, there can be a market.**
