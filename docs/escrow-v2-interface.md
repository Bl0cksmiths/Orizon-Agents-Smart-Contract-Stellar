# PaymentEscrow v2 — frozen interface

Status: **frozen** for the 5.01 build lanes (contract, backend, frontend, harness).
Change it only by a commit to this file, announced to every lane.

## Why v2 exists

v1 (`CBJPTMAP…25PI` on testnet) cannot settle, for three independent reasons:

1. `charge` calls `usdc.transfer(&auth.payer, …)`, which needs the **payer's**
   authorization, but only the settler signs a charge. Testnet rejects every
   one with `Error(Auth, InvalidAction)` (contracts issue #3, defect D-039).
   The v1 unit tests passed only because they ran under
   `mock_all_auths_allowing_non_root_auth()`, which the network never does.
2. The settler is written once at construction (`GA7AI5…`) and has no setter,
   while the deployment signs as `GDB4N25…`.
3. `charge` pays `owner_of(auth.agent_id)`, and the console authorizes a whole
   plan as `orizon_batch`, whose owner is the settler. An external operator
   could never be paid.

v2 fixes all three by taking **custody** at `authorize`, paying **each step's
operator** at `settle`, and letting the admin **rotate the settler**.

## Entry points

```rust
fn __constructor(env, admin: Address, usdc: Address, registry: Address, settler: Address);

/// UNCHANGED SIGNATURE from v1, so every transaction builder keeps working.
/// `agent_id` is now a label for the authorization, and the label is the
/// PLAN ID the buyer is paying for (`pln_` + 8 hex, a valid Symbol). The
/// backend refuses to execute a plan against an authorization whose label,
/// payer, cap or state does not match (finding S2); payouts name their own
/// agents at `settle`.
/// payer.require_auth(); max_amount > 0 (BadAmount); expires_at > now (Expired).
/// Moves `max_amount` payer -> this contract in the same invocation, so the
/// payer's one signature covers both.
fn authorize(env, payer: Address, agent_id: Symbol, max_amount: i128, expires_at: u64)
    -> Result<BytesN<16>, Error>;

/// Settler only (caller.require_auth() and caller == settler, else Unauthorized).
/// Refused when the authorization is missing (NotFound), reclaimed (Revoked),
/// or already settled (Replay). NOT refused for being past `expires_at`
/// (amended for 5.01, finding S3): a long run must still pay the operators it
/// used. The payer is protected by `reclaim`, which is allowed only after
/// expiry, and whichever of `settle` and `reclaim` lands first wins.
/// `payouts`: 0..=16 entries (else BadPayouts); every amount > 0 (BadAmount);
/// their sum <= max_amount (Insufficient).
/// For each payout: pay owner_of(agent_id) from custody, write a Receipt,
/// emit `charged`. Then return (max_amount - sum) to the payer, mark the
/// authorization settled, emit `settled`. All in one transaction.
/// An empty `payouts` is a full release: nothing was delivered, everything
/// goes back to the payer.
/// A payout naming an agent the AgentRegistry does not hold makes the
/// `owner_of` cross-call trap, and the WHOLE settle reverts with a host error
/// (not a contract code). The seeded `agt_*` catalogue is not on-chain, so the
/// backend must leave any step without an on-chain owner out of `payouts`.
/// Returns the receipt ids, in `payouts` order.
fn settle(env, caller: Address, auth_id: BytesN<16>, job_id: BytesN<16>, payouts: Vec<Payout>)
    -> Result<Vec<BytesN<16>>, Error>;

/// The payer takes custody back from an authorization that was never settled.
/// payer.require_auth(); payer == auth.payer (Unauthorized); not settled
/// (Replay); not already reclaimed (Revoked); only once now > expires_at
/// (Locked) — before then the settler may still owe operators for delivered work.
/// Returns the amount returned. Emits `reclaimd`.
fn reclaim(env, payer: Address, auth_id: BytesN<16>) -> Result<i128, Error>;

/// Admin only. Emits `settler`.
fn set_settler(env, new_settler: Address) -> Result<(), Error>;

// Views
fn authorization(env, auth_id: BytesN<16>) -> Result<Authorization, Error>;
fn receipt(env, receipt_id: BytesN<16>) -> Result<Receipt, Error>;
fn settler(env) -> Address;
fn admin(env) -> Address;
fn version(env) -> u32; // 2
```

`charge` and `revoke` are **removed**. A v1 caller gets a host "function not
found" error, never a half-working call.

## Types

```rust
// payment-escrow crate
#[contracttype] pub struct Payout { pub agent_id: Symbol, pub amount: i128 }

// orizon-shared (Authorization gains one field; field order below is final)
pub struct Authorization {
    pub payer: Address, pub agent_id: Symbol, pub max_amount: i128,
    pub spent: i128, pub expires_at: u64, pub revoked: bool, pub settled: bool,
}
// Receipt is unchanged: { auth_id, agent_id, amount, job_id, settled_at }
```

`spent` is the sum paid out at `settle`. `revoked` means reclaimed by the payer.

## Errors (u32, shared with `orizon_shared::codes`)

| Code | Name | Meaning |
|---|---|---|
| 1 | Unauthorized | caller is not the settler / admin / the authorization's payer |
| 2 | NotFound | no such authorization or receipt |
| 4 | Expired | `authorize` with `expires_at <= now` |
| 5 | Insufficient | payouts sum past `max_amount` |
| 6 | Revoked | the payer already reclaimed it |
| 7 | Replay | already settled |
| 9 | Locked | `reclaim` before `expires_at` has passed |
| 101 | BadAmount | a non-positive `max_amount` or payout amount |
| 102 | BadPayouts | more than 16 payouts |

## Events

| Topics | Data | When |
|---|---|---|
| `("authd", agent_id)` | `(auth_id, payer, max_amount)` | authorize — unchanged from v1 |
| `("charged", payout.agent_id)` | `(receipt_id, auth_id, amount, job_id)` | once per payout — **same shape as v1**, but the topic is now the agent actually paid |
| `("settled",)` | `(auth_id, job_id, spent, returned)` | once per settle |
| `("reclaimd",)` | `(auth_id, payer, returned)` | reclaim |
| `("settler",)` | `(old, new)` | set_settler |

## What does not change

Refunds for upheld disputes stay a settler-funded SAC transfer (ADR 0002), and
ratings and seals are untouched. v1's four published testnet ids stay in the
evidence as history; v2 is added beside them and the switch is disclosed.
