# Orizon Agents — Smart Contracts (Stellar / Soroban)

Four Rust contracts that put the Orizon Agents stack on-chain:

## 🚀 Live deployment

| layer | live URL | source |
| --- | --- | --- |
| 🔗 **Soroban contracts** (this repo, Stellar mainnet + testnet) | 4 contracts deployed — [see addresses ↓](#current-testnet-deployment) | this repo |
| 🌐 **Frontend** (Vercel) | **https://orizon-agents-fe-stellar.vercel.app** | [Frontend repo](https://github.com/ALGOREX-PH/Orizon-Agents-FE-Stellar) |
| ⚙️ **Backend** (Render) | **https://orizon-agents-be-stellar.onrender.com** | [Backend repo](https://github.com/ALGOREX-PH/Orizon-Agents-BE-Stellar) |

**▸ See the contracts in action:** [open the dApp](https://orizon-agents-fe-stellar.vercel.app/app/orchestrator) → connect [Freighter](https://freighter.app) on **Test Net** → type `code a calculator web app` → **Authorize & Execute**. The trace ends with two real testnet transactions calling `PaymentEscrow.charge` and `AttestationRegistry.seal`, both linked to `stellar.expert`.

### Current testnet deployment

| contract | id |
| --- | --- |
| `AgentRegistry`        | [`CAPHXWU5…J3GQ`](https://stellar.expert/explorer/testnet/contract/CAPHXWU53UZUZJGV7IAE57NNMH3YYB5MTWO6YA53KKMXSFVLOITBJ3GQ) |
| `PaymentEscrow` (x402) | [`CBJPTMAP…525PI`](https://stellar.expert/explorer/testnet/contract/CBJPTMAPMGODGZCZ2IMEQSRUX3WGUXNMKDTNN2KMJ3NFGYZ5OJ5525PI) |
| `AttestationRegistry`  | [`CBYUZKOE…HEGK`](https://stellar.expert/explorer/testnet/contract/CBYUZKOET43UXTBXZUJIBBJW5ODGD2J2AZVVXCR3QONGOCAHOXQQHEGK) |
| `ReputationLedger`     | [`CDCSOBEV…22ZT`](https://stellar.expert/explorer/testnet/contract/CDCSOBEVZUPQZV5GV4D6KYHZCLNGW2KXY74RUHSZ3EZUXF34DPW422ZT) |
| Asset SAC (XLM)        | [`CDLZFC3S…CYSC`](https://stellar.expert/explorer/testnet/contract/CDLZFC3SYJYDZT7K67VZ75HPJVIEUVNIXF47ZG2FB2RMQQVU2HHGCYSC) |

Admin: `GA7AI5TAJEZA27I666DSJC4MUJYBEWUYNNZWPU7R2ONA7IZQVO6R5OQV`

### Current mainnet deployment

| contract | id |
| --- | --- |
| `AgentRegistry`        | [`CBTJ3BXT…LTD4`](https://stellar.expert/explorer/public/contract/CBTJ3BXTMTA2PQLRTSAZHEWQRTBMNHYCOKY5WOIYAH36LT4HTN63LTD4) |
| `PaymentEscrow` (x402) | [`CBJCQBA4…5CNF`](https://stellar.expert/explorer/public/contract/CBJCQBA47Q3EQ7HC46GAWJPVM7KMD5KAEI5KG4FPYJFKR3NYB4QR5CNF) |
| `AttestationRegistry`  | [`CBLV6QGF…AAK4`](https://stellar.expert/explorer/public/contract/CBLV6QGFCMXBXHT62JZ7YH22NXW7MVBGV6TGOGX3OHY46GQGPYCTAAK4) |
| `ReputationLedger`     | [`CDFWQJY7…AXSX`](https://stellar.expert/explorer/public/contract/CDFWQJY72GPH7PEQVFGBDZESZNVRF6LQLVWU42CFMWPGRME5RWN5AXSX) |
| Asset SAC (XLM)        | [`CAS3J7GY…OWMA`](https://stellar.expert/explorer/public/contract/CAS3J7GYLGXMF6TDJBBYYSE3HQ6BBSMLNUQ34T6TZMYMW2EVH34XOWMA) |

Admin: `GA7AI5TAJEZA27I666DSJC4MUJYBEWUYNNZWPU7R2ONA7IZQVO6R5OQV`

---


| crate | purpose |
| --- | --- |
| `agent-registry` | ERC-8004-style identity, skills, price catalog |
| `reputation-ledger` | decayed, value-weighted rating evidence per agent (v2) |
| `payment-escrow` | x402-style USDC escrow (v2): custody at authorize, per-operator payouts at settle, payer reclaim |
| `attestation-registry` | write-once workflow receipts (job_id → proof record) |

Target: **Stellar mainnet** (production) + **testnet**, Protocol 22+. Payments settle in **USDC** via the Stellar Asset Contract (SEP-41).

### ReputationLedger v2

Decayed, value-weighted, dispute-aware evidence store (Jøsang beta-reputation with a forgetting factor; ERC-8004 convention of raw evidence on-chain, complex aggregation off-chain):

- `submit(caller, agent_id, job_id, rating_0_to_100, weight, payer, kind)` — scorer-only. `weight` is the job's USDC value in stroops, capped at 100 USDC per rating; the `(agent, job)` replay guard lives in **persistent** storage (v1 kept it in temporary storage, which expires). `kind = "dispute"` also bumps the lifetime dispute counter.
- Evidence decays by λ = 0.925 per weekly epoch (≈ 9-week half-life), applied lazily; after 96 idle epochs it is fully forgotten. Lifetime `count` / `disputed` never decay.
- Views (all decay-to-now, read-only): `rep_state`, `avg_bps` (weighted mean, basis points), `rep_bps(prior_bps, prior_weight)` (Bayesian-smoothed toward a caller-supplied prior), `dispute_rate_bps`, `payer_weight` (cumulative per-payer stake for off-chain Sybil analysis).

### PaymentEscrow v2

Frozen interface: [`docs/escrow-v2-interface.md`](docs/escrow-v2-interface.md). v1 (`CBJPTMAP…25PI`) can never settle on-chain: its `charge` moved funds out of the payer's balance under only the settler's signature, its settler was fixed at construction, and it paid the authorization's label rather than each step's operator. v2 fixes all three:

- `authorize(payer, agent_id, max_amount, expires_at)` — signature unchanged from v1. Moves `max_amount` from the payer into the contract's **custody** in the same invocation, so the payer's one auth entry covers the root call and its nested SAC `transfer`. `agent_id` is now only a label: the plan id being paid for (`pln_` + 8 hex). Rejects `max_amount <= 0` (BadAmount) and `expires_at <= now` (Expired).
- `settle(caller, auth_id, job_id, payouts)` — settler only. Up to 16 `Payout { agent_id, amount }`, each `> 0`, summing to at most `max_amount`. Pays each `owner_of(agent_id)` from custody with a receipt and a v1-shaped `charged` event (topic = the agent actually paid), returns the remainder to the payer, marks the authorization `settled`. An empty list is a full release. State is written before any transfer. **Not refused after `expires_at`**, so a long run still pays the operators it used; it is refused only for an unknown (NotFound), reclaimed (Revoked) or already-settled (Replay) authorization. A payout to an agent the registry does not hold reverts the whole settle with a host error.
- `reclaim(payer, auth_id)` — the payer takes the full `max_amount` back from an unsettled authorization, only once `now > expires_at` (Locked before then). After expiry, whichever of `settle` and `reclaim` lands first wins: the other gets Replay or Revoked.
- `set_settler(new_settler)` — admin only; the settler can be rotated without a redeploy.
- Views: `authorization`, `receipt`, `settler`, `admin`, `version` (= 2). `charge` and `revoke` are gone.
- Storage: the instance is kept at 30 days' TTL; every Auth and Receipt entry is extended to 30 days on each write and read, and an open authorization additionally for its whole window, so it cannot be archived before it is settled or reclaimed, including by a settle that lands after expiry.

The tests run every money path under an explicit authorization tree (`env.mock_auths`, asserted with `env.auths()`), never `mock_all_auths*`, against a real Stellar Asset Contract and the real AgentRegistry.

Deploy v2 alone on testnet, beside the existing contracts (reuses the registry and asset SAC in `addresses.json`, records `payment_escrow_v2` there, keeps v1's id as history):

```bash
make deploy-escrow-v2 SETTLER=G...        # SOURCE=admin by default; the source's address becomes the escrow admin
```

## One-time setup

```bash
rustup target add wasm32-unknown-unknown

# Easiest install: pre-built binary from GitHub releases
mkdir -p ~/.local/bin
curl -L https://github.com/stellar/stellar-cli/releases/download/v26.0.0/stellar-cli-26.0.0-x86_64-unknown-linux-gnu.tar.gz \
  | tar xz -C ~/.local/bin/
chmod +x ~/.local/bin/stellar
stellar --version                       # → stellar 26.x.x

# Identity (v26: no --global flag — identities are global by default)
stellar keys generate admin --network testnet --fund
stellar keys address admin              # your deployer G-address
```

> ⚠️ Don't `apt install seqan-apps`. Ubuntu's seqan-apps package ships
> an unrelated `stellar` binary that will shadow this one. If you've
> installed it, remove with `sudo apt remove --purge seqan-apps`.

## Common commands

```bash
make check         # cargo check --all
make test          # cargo test --all
make build         # stellar contract build → target/wasm32-unknown-unknown/release/*.wasm
make deploy-test   # deploys all four to testnet; writes addresses.json
make deploy-main   # deploys all four to mainnet (CONFIRM_MAINNET=yes guard); writes addresses.mainnet.json
make deploy-escrow-v2 SETTLER=G...  # deploys only PaymentEscrow v2 to testnet; adds payment_escrow_v2 to addresses.json
```

Per-network address books (`addresses.json` for testnet, `addresses.mainnet.json` for mainnet) are gitignored.

## Storage lifetime (TTL)

Soroban archives a contract's instance, its wasm and each persistent entry once its TTL runs out, and a read of an archived entry fails until it is restored. The registries and the ledger extend what they touch (defect D-083):

- `AgentRegistry`, `ReputationLedger`, `AttestationRegistry`: every write and every read extends the instance (which carries the wasm) and the records involved to 180 days. That's the network maximum today, and the contracts clamp to whatever the live maximum is. An entry is only re-extended once it has less than 179 days left, so rent is paid at most about once a day per entry. Helpers: `orizon_shared::ttl`.
- `PaymentEscrow` v2: 30 days, see above.

Only a call inside a **submitted** transaction extends anything. The backend and the site read by simulation, which changes nothing on chain, and the contracts deployed before this change don't extend at all. Keep everything alive with the keeper script:

```bash
make ttl-check                     # read-only: every contract in addresses.json, before live-until, and the commands it would run
make ttl-extend SOURCE=ttl-keeper  # extends the instance, wasm and persistent entries that have < 150 days left, to ~173 days
python3 scripts/extend_ttl.py --help   # --restore-archived, --keys-file, --network mainnet --rpc-url …, thresholds
```

It finds each contract's persistent entries through the stellar.expert contract-data index and reads every live-until from Soroban RPC. `--keys-file` adds keys by hand, and `--no-discover` skips the index. Extending needs no contract role: any funded account can pay, so use a throwaway identity (`stellar keys generate ttl-keeper --network testnet --fund`). It's idempotent, and a run straight after another does nothing. Archived entries are reported and left alone unless `--restore-archived` is passed.

Run it on a schedule well inside the 150-day renew window, e.g. weekly from any machine that holds the identity:

```cron
# crontab -e   (Mondays 03:00 UTC)
0 3 * * 1  cd /path/to/Orizon-Agents-Smart-Contract-Stellar && python3 scripts/extend_ttl.py --apply --source ttl-keeper --quiet >> ~/orizon-ttl.log 2>&1
```

A systemd timer or any CI scheduler with the identity's secret (`STELLAR_ACCOUNT`) works the same way. The repo deliberately ships no workflow for it.

## Job lifecycle (on-chain)

```
authorize(payer, agent_id, max, expires)  → auth_id       ← PaymentEscrow (payer → custody)
settle(caller, auth_id, job_id, payouts)  → receipt_ids   ← PaymentEscrow (custody → each operator, rest → payer)
seal(caller, job_id, orchestrator,        → ()           ← AttestationRegistry
     intent_hash, agents, receipts, total_spent)
submit(caller, agent_id, job_id, rating,  → ()           ← ReputationLedger
       weight, payer, kind)
```

The backend (FastAPI + Agno) orchestrates the intent, calls these contracts in order, and streams the SSE trace to the frontend.

## Layout

```
contract/
  shared/                 # #[contracttype] structs shared across contracts
  agent-registry/
  reputation-ledger/
  payment-escrow/
  attestation-registry/
scripts/
  deploy_testnet.sh       # deploys everything, outputs addresses.json
  fund_accounts.sh        # friendbot for local test accounts
  extend_ttl.py           # storage-lifetime keeper (dry run by default)
```

MVP contracts are **not upgradable**. Re-deploy on logic changes.
