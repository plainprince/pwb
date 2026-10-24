# Design — §5 Dynamic Rolling Average Domain Pricing & Gossip Merging (`pwb-node::domain`)

## Objective

Replace any fixed claim cost with a dynamic, rolling-average pricing formula based
on rolling network metrics, and deterministically reconcile state when two isolated
P2P network clusters merge.

## Components

### `pwb-node::domain` — Rolling Average Pricing Engine

`RollingPricing` keeps a window (`PRICING_WINDOW=8`) of per-epoch
`PricingEpochStats { epoch, registrations, network_work, node_count }`
(gossip-delivered; stale / duplicate epochs are ignored).

Each `record_epoch` recomputes integer-only rolling averages and moves the price
¼ of the way toward `price × demand / supply`:

- **Demand** = recent registration flow vs a balanced target
  (`TARGET_REGS_PER_EPOCH=4`, scaled by fixed-point `PRICE_SCALE=1000`)
- **Supply** = per-node work capacity × network breadth
  (recent credits minted split as `network_work / node_count` vs `BASELINE_WORK_PER_NODE=1000`
  and `node_count` vs `BASELINE_NODES=10` — the two factors multiply back to total
  work vs the balanced baseline, so both inputs enter the price)

Clamped to `[MIN_CLAIM_PRICE=10, MAX_CLAIM_PRICE=10_000]`. A fresh engine starts at
`INITIAL_CLAIM_PRICE=10`: indexing a hosted bundle of at least 640 compressed
bytes clears the 10-credit traffic floor and pays for a first claim. A smaller
bundle must earn the rest from served queries or relayed cells; `pwb-node`
keeps serving and retries a deferred claim.

#### API

- `DomainRegistry` embeds the engine; `claim_cost()` returns the current price
- `register_attested` / `register_blind` charge `claim_cost()` instead of a caller-supplied cost (cert balance and voucher denomination are checked against the live price)
- `record_pricing_epoch()` folds one epoch of network-wide stats into the rolling price

| Constant | Value | Meaning |
|----------|-------|---------|
| `INITIAL_CLAIM_PRICE` | 10 credits | Fresh engine starting price |
| `MIN_CLAIM_PRICE` | 10 credits | Lower bound |
| `MAX_CLAIM_PRICE` | 10,000 credits | Upper bound |
| `PRICING_WINDOW` | 8 epochs | Observations retained |
| `TARGET_REGS_PER_EPOCH` | 4 | Balanced registration flow |
| `BASELINE_WORK_PER_NODE` | 1,000 | Per-node credits minted per epoch at balance |
| `BASELINE_NODES` | 10 | Node count at balance |
| `PRICE_SCALE` | 1,000 | Fixed-point scale: 1,000 = balanced |
| `ADJUSTMENT_DIVISOR` | 4 | Share of gap closed per epoch |

### `pwb-node::domain` — Gossip Partition & Network Merge State Reconciliation

Deterministic state reconciliation when two isolated P2P network clusters merge
(gossip sync), using Conflict-Free Replicated Data Types (CRDTs).

#### `RollingPricing::merge`

- Unions per-epoch observations (per-field *maximum* on shared epochs, so re-gossiping the same window never inflates demand / supply)
- Keeps the most recent `PRICING_WINDOW` epochs
- Takes the maximum observed base cost on either side (a cost established in either partition is never silently lowered)
- Commutative, associative, idempotent

#### `DomainRegistry::merge`

Resolves conflicting claims:
- Last-writer-wins by `registered_at_epoch` (owner public key bytes break ties)
- Repairs overdrawn attested funding groups: every record citing the same `(owner, funding.head)` spends from one attested state, so groups whose live spend exceeds the cited balance evict their lowest-priority records (oldest epoch first, then domain name — first-spent wins), plus a cumulative per-owner pass across heads. Attested spend accounting is derived from the surviving records themselves, so one certificate cannot fund two surviving claims even across partitions
- Unions the voucher spend log
- Merges rolling pricing

#### `VoucherSpendLog::merge` (`pwb-core::crypto`)

Unions spent-token sets so a bearer voucher cannot be redeemed twice across the re-joined network.

#### New derives

`Clone` / `PartialEq` / `Eq` on `DomainRecord`, `DomainRegistry`, `RollingPricing`, `VoucherSpendLog`.
