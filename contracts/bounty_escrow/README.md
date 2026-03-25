# Bounty Escrow Contract

## Batch Operations & Failure Semantics

The escrow contract supports atomically locking or releasing multiple bounties in a single transaction via `batch_lock_funds` and `batch_release_funds`.

### All-or-nothing atomicity

A batch either succeeds completely or reverts entirely — no partial state is written. If any item fails validation (wrong amount, duplicate ID, bounty already exists, etc.) the transaction panics and every sibling item is rolled back.

### Processing order

Items are sorted by ascending `bounty_id` before execution, ensuring deterministic ordering regardless of input order. This makes replay and debugging reproducible across runs.

### Limits

| Constant | Value | Error when exceeded |
|---|---|---|
| `MAX_BATCH_SIZE` | 20 | `InvalidBatchSize` |
| Duplicate `bounty_id` within one batch | — | `DuplicateBountyId` |

### Common failure causes

| Error | Triggered when |
|---|---|
| `NotInitialized` | Contract `init()` has not been called |
| `FundsPaused` | Lock/release operations are paused by admin |
| `ContractDeprecated` | Contract has been deprecated (lock only) |
| `InvalidBatchSize` | Batch is empty or exceeds `MAX_BATCH_SIZE` |
| `DuplicateBountyId` | Same `bounty_id` appears more than once in the batch |
| `InvalidAmount` | An `amount` field is `<= 0` or outside the configured `AmountPolicy` |
| `BountyExists` | A `bounty_id` in `batch_lock_funds` is already in storage |
| `BountyNotFound` | A `bounty_id` in `batch_release_funds` does not exist |
| `FundsNotLocked` | Escrow for that ID exists but is not in `Locked` status |

For full details including the CEI pattern breakdown, reentrancy guarantees, and security assumptions, see [contracts/escrow/BATCH_SEMANTIC.md](contracts/escrow/BATCH_SEMANTIC.md).

---

## Dry-Run Simulation API

The escrow contract provides read-only dry-run entrypoints for previewing operations without mutating state. See [contracts/escrow/DRY_RUN_API.md](contracts/escrow/DRY_RUN_API.md) for full documentation.

- **dry_run_lock** – Simulate lock without transfers
- **dry_run_release** – Simulate release without transfers
- **dry_run_refund** – Simulate refund without transfers

All return `SimulationResult` with success/error_code/amount/resulting_status/remaining_amount. No authorization required.

## Metadata Constraints

The escrow metadata API enforces validation rules for human-readable tags like
`bounty_type`. See `contracts/escrow/METADATA_CONSTRAINTS.md` for the current
limits and guidance.

---

# Soroban Project

## Project Structure

This repository uses the recommended structure for a Soroban project:
```text
.
├── contracts
│   └── hello_world
│       ├── src
│       │   ├── lib.rs
│       │   └── test.rs
│       └── Cargo.toml
├── Cargo.toml
└── README.md
```

- New Soroban contracts can be put in `contracts`, each in their own directory. There is already a `hello_world` contract in there to get you started.
- If you initialized this project with any other example contracts via `--with-example`, those contracts will be in the `contracts` directory as well.
- Contracts should have their own `Cargo.toml` files that rely on the top-level `Cargo.toml` workspace for their dependencies.
- Frontend libraries can be added to the top-level directory as well. If you initialized this project with a frontend template via `--frontend-template` you will have those files already included.

## Bounty Escrow Auto-Refund Rules

For `contracts/escrow`, automated refund triggering is permission-restricted:

- `refund(bounty_id)` requires authenticated authorization from both:
  - Contract `admin`
  - Escrow `depositor`
- Refund eligibility is still governed by existing rules:
  - Deadline-based refund when `now >= deadline`, or
  - Admin-approved refund via `approve_refund`
- Unauthorized callers cannot trigger refund execution.

See:

- `contracts/escrow/src/lib.rs` (`refund`)
- `contracts/escrow/src/test_auto_refund_permissions.rs`
- `contracts/escrow/AUTO_REFUND_TESTS.md`
- `SECURITY.md`
