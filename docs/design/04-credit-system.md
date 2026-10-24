# Design — §4 Anti-Sybil Defense & Peer-Attested Credit System (`pwb-node::credit`)

## Objective

Prevent Sybil attacks and large-miner hardware takeovers by requiring credits to
be earned through verified network service delivery, with balances attested by
peer neighborhoods rather than self-declared.

## Components

### `pwb-node::credit` — Proof-of-Time & Useful Work Credit Minting

Earn credits strictly by delivering verified network services over time:
serving `search.plain` queries, indexing PML bundles, relaying onion cells.

- `CreditMinter` is bound to one owner `PublicKey` and mints only that identity's receipts (`MintError::WrongSigner` otherwise)
- Three service kinds — each with domain-separated signed receipts:
  - `QueryReceipt` (requester challenge nonce, TTL-bound, one claim via `ChallengeLedger`)
  - `IndexReceipt` (BLAKE3 content id over the exact bundle bytes + `canonical_term_hash` re-derived from the stored terms, shared with `search::extract_terms`)
  - `RelayReceipt` (window number, cell count, byte count, and chain head of the cell hash chain)

#### Mint gate order

epoch rollover → per-service cooldown → traffic floor → epoch cap, then accounting.
`authorize` consumes `amount × traffic_per_credit` from uncredited verified bytes so
credits can never outrun observed network flow (the anti-ASIC floor).
Relay credits pay at whole-KiB floor (1 credit/KiB default) so relaying always backs itself 1:1.

#### Relay windows

- `relay_cell` folds every cell into a live BLAKE3 chain and records its bytes
- `seal_relay_window` seals (window monotone, claim-once, refuses empty seals or overwriting an unclaimed seal)
- `mint_relay` requires a signature-valid receipt whose window / cells / bytes / chain-head exactly match the sealed state (`RelayChainBroken` on well-signed mismatch)

### `pwb-node::ledger` — Sharded Peer-Attested Ledger ($k$-of-$N$ Neighborhoods)

Nodes do **not** store or know their own authoritative credit balances; credit
history is attested and stored by $N$ deterministic peer nodes
(`BLAKE3(NodeID || Epoch)` shard coordinate).

- `epoch_key` derives the per-epoch shard coordinate `BLAKE3(NodeID || Epoch)` (domain-separated)
- `select_attestors` deterministically ranks candidate peers by XOR distance to the subject's shard key — membership rotates every epoch and the subject is never its own attestor
- Each attestor runs an `AttestorLedger`: a per-subject hash-chained credit history (`Earned` / `Spent` events folded via `chain_head_after`, verified by `verify_history`) that refuses:
  - Self-recording (`SelfAttestation`)
  - Epoch regression
  - Duplicate evidence
  - Overdrafts
- `record_index_earned` re-verifies the §4.1 index receipt end-to-end (signature, content id, canonical term hash) against the actual bundle bytes before extending the chain
- `attest` signs a `BalanceAttestation` (subject, epoch, balance, chain head) with the attestor's key

#### Quorum certificates ($k$-of-$N$)

`QuorumCertificate` aggregates $k$ attestations over one identical statement;
`verify` enforces:
- Quorum size
- Neighborhood membership
- Signer distinctness
- No self-vouching
- Every signature (a verifiable $k$-of-$N$ quorum of per-attestor Ed25519 signatures — any $k$ members must independently vouch; no new dependencies)

By default the neighborhood is the deterministic attestor set derived from the
receiver's live peers (`DEFAULT_QUORUM_K = 5`, shrinking to the known peer
count). That is circular for a fresh node: its only peer could supply a
self-consistent certificate. `DomainService::new_with_trust` replaces the peer
view with operator-pinned anchors (`--trust-anchor` / `PWB_TRUST_ANCHORS`); the
threshold becomes `min(5, anchors)`, and both gossip and registry sync verify
against that fixed set. The anchor identities must be published independently
of the seed link for the pin to mean anything.

`DomainRegistry::register_attested` claims a `.pwb` domain only against a verified certificate, checks reserved/duplicate rules, and enforces the attested budget against spend derived from the live funded records (`InsufficientAttestedBalance` on overdraw). Every attested record cites its funding — the certificate's `(head, balance)` — and the owner's signature commits to that citation (`PWBDOMAIN2`), so the citation cannot be stripped or swapped without invalidating the record. The engine's payload check and every claim path (`claim_attested`, `on_attested_claim`, registry sync) enforce the same funding-bound verification, and merges repair overdrawn funding groups (see §5). The per-owner `claimed` side table is gone: because funding travels on the records, spend accounting survives restarts and converges on merge (one record counts once, however many partitions gossiped it).

### `pwb-node::blind` — Blind Balance & Privacy Preservation

Layering the Chaum-style blind Schnorr vouchers of `pwb-core::crypto` over the §4.2 attested ledger, so peer attestors log credit volume without linking node owners to specific browsing actions or domain claims.

#### Issuer side

- `BlindBalanceIssuer` derives one independent signing key per denomination from a master seed (`from_denomination_seed`, so amounts cannot be forged)
- Pairs committed single-use nonces positionally with incoming blinded requests:
  - `commit_nonces` publishes commitment points
  - `sign_batch` validates count / balance / denomination *before* consuming nonces (refused batches never burn nonces), then signs blind

#### Requester side

- `BlindWithdrawal::prepare` rebuilds each `BlindVoucherNonce` from its wire point (`BlindVoucherNonce::from_point`, new in `pwb-core::crypto` — placeholder scalar, never signable), blinds fresh secrets
- `finish` unblinds partial signatures into `WithdrawnVoucher`s that carry the bearer secret

#### `withdraw_attested`

Composes the full protocol against one attestor's ledger:
1. Checks the subject's attested balance
2. Runs the commit → blind → sign → unblind roundtrip
3. Records a `Spent` volume event whose evidence digest (`withdrawal_evidence`, domain-separated `PWBBLIND1`) is derived only from amounts and blinded challenges — *never voucher tokens* — so the logged volume stays unlinkable to the eventual spends
4. Fresh blinding makes every withdrawal's digest unique

Because the balance is attested by $N$ peers, callers must gossip the same evidence (`WithdrawalReceipt::evidence`) to sibling ledgers via `record_spent` (cross-attestor inflation guard).

#### Redemption

- `IssuerBinding` is the attestor's Ed25519 signature over a canonical `PWBBLIND1‖binding` encoding binding one denomination key to its identity for an epoch
- `BlindClaim` = voucher + zero-knowledge `VoucherOwnershipProof` + binding
- `DomainRegistry::register_blind` (with a `VoucherSpendLog`) verifies:
  - Reserved / duplicate rules
  - The binding signature
  - That the binding's attestor is a neighborhood member (only *attested* credits are privately spendable)
  - The voucher against the bound denomination key
  - The ownership proof
  - The denomination against the claim cost (`Underfunded` otherwise)
  - Records the token so a voucher cannot fund a second claim (`AlreadySpent`)
- The record's owner is the redeemer's fresh *pseudonym* key — unlinkable to the earning identity that withdrew
