# Design — §1 Core Architecture Restructuring (`pwb-core` Consolidation)

## Objective

Consolidate shared protocol logic into `pwb-core` so both `pwb-browser` and
`pwb-node` run on the exact same engine. Remove duplicate code between the
two crates.

## Components

### `pwb-core::net` — Network Engine

- Peer table management
- Routing logic
- Framing serialization
- Socket event loops
- Both `pwb-browser` and `pwb-node` instantiate `pwb_core::net::NodeEngine`

### `pwb-core::crypto` — Cryptographic Primitives

- Keypairs: `Ed25519` for signatures, `X25519` for ECDH key exchange, `BLAKE3` for content hashing/IDs
- Zero-knowledge / blind signature vouchers for credit attestations:
  - Unlinkable Chaum-style blind Schnorr signatures over Ristretto
  - Types: `BlindVoucherIssuer`, `BlindVoucher`, `BlindVoucherPublicKey`, `VoucherSecret`, `VoucherBlinding`
  - The issuer never sees the token; redemption is publicly verifiable without revealing the bearer secret
- Zero-knowledge ownership proofs: `VoucherOwnershipProof`, `BlindVoucher::prove_ownership` / `verify_ownership`
- Per-denomination issuer keys: `BlindVoucherIssuer::from_denomination_seed` (reject out-of-denomination requests)
- Single-round issuance via pre-generated nonce pools (`nonce_pool`)
- Replay rejection: `VoucherSpendLog`, `VoucherSpendError`

### `pwb-core::system` — Unified System Protocol Resolver

Centralize native `.plain` system domain handlers inside `pwb-core::system`:

- `search.plain`, `devtools.plain`, `register.plain`, `gossip.plain`, `nodes.plain`, `network.plain`, `help.plain`
- Single source of truth for:
  - Reserved-domain list
  - `.plain` = official namespace rule (system protocols + shipped content pages); `.pwb` = unofficial user namespace
  - `plain://` URL parsing (`SystemUrl` / `SystemDomain` with `q=` / `connect=` parameter extraction)
  - `nodes.plain` connection-link codec (`build_connection_link` / `parse_connection_link`, previously inlined in `NodeEngine`)

### Cross-crate delegation

- `pwb-node::domain` re-exports the canonical reserved-domain list and rule
- `NodeEngine`'s link methods delegate to `pwb-core::system`
- `pwb-node::system` renders every native domain, including `gossip.plain`, `nodes.plain` (connection links render as a connect card), `network.plain` (live addresses, peers, and local-service counts), and `help.plain`
- `pwb-browser`'s resolver dispatches all seven domains through the core parser; `network.plain` receives a `NetworkSnapshot` (listen/discovery/STUN addresses plus the live peer table) from the owning app
