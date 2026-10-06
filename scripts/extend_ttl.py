#!/usr/bin/env python3
"""Keep the deployed contracts' storage from being archived (D-083).

Soroban archives a contract's instance, its wasm and each persistent entry
once that entry's TTL runs out; a read of an archived entry then fails until
someone restores it. This script reads every contract in the address book,
reports how long each of those entries has left, and (with --apply) extends
the ones that are running low. Any funded account can pay for an extension;
no contract role or signature is involved.

Dry run by default: it only reads (Soroban RPC getLedgerEntries) and prints
the stellar-cli commands it would run. With --apply it runs them, then reads
everything again and prints the new live-until ledgers. Re-running is safe:
an entry is only extended while it has less than --renew-below-days left, so
a second run straight after the first does nothing.

Which entries:
  * every contract id in the address book: its instance, and its wasm (a
    Stellar Asset Contract has no wasm);
  * every persistent entry of those contracts, listed by the stellar.expert
    contract-data index (the asset SAC's entries are other accounts'
    balances and are skipped). Each one's live-until is then read from RPC,
    which is authoritative. --no-discover skips the index;
  * any extra keys in --keys-file, one per line: "<address-book-field>
    <base64 ScVal key>", '#' starts a comment.

Archived entries are reported. They need a restore transaction first, which
--restore-archived adds (restore, then extend, in the same run).

Usage:
  python3 scripts/extend_ttl.py                                  # testnet dry run
  python3 scripts/extend_ttl.py --apply --source ttl-keeper      # extend
  python3 scripts/extend_ttl.py --network mainnet --rpc-url https://...  # mainnet dry run

Requires python3 (standard library only) and, for --apply, stellar-cli v23+
with the --source identity funded on that network.
"""

from __future__ import annotations

import argparse
import base64
import datetime as dt
import json
import shlex
import struct
import subprocess
import sys
import time
import urllib.error
import urllib.request
from dataclasses import dataclass
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
LEDGER_SECONDS = 5
DAY_IN_LEDGERS = 17_280
# Keys per extend/restore transaction; the network allows 400 footprint entries.
BATCH = 100
USER_AGENT = "orizon-extend-ttl/1"

NETWORKS = {
    "testnet": {
        "book": "addresses.json",
        "rpc": "https://soroban-testnet.stellar.org",
        "passphrase": "Test SDF Network ; September 2015",
        "expert": "testnet",
    },
    "mainnet": {
        "book": "addresses.mainnet.json",
        "rpc": None,  # SDF runs no public mainnet RPC: pass --rpc-url.
        "passphrase": "Public Global Stellar Network ; September 2015",
        "expert": "public",
    },
}

# Address-book fields whose contracts only need their instance kept alive.
INSTANCE_ONLY = {"asset_sac"}

# XDR discriminants (Stellar-ledger-entries.x, Stellar-contract.x).
LEDGER_ENTRY_CONTRACT_DATA = 6
LEDGER_ENTRY_CONTRACT_CODE = 7
SC_ADDRESS_CONTRACT = 1
SCV_LEDGER_KEY_CONTRACT_INSTANCE = 20
SCV_CONTRACT_INSTANCE = 19
SCV_BYTES = 13
SCV_SYMBOL = 15
SCV_VEC = 16
SCV_ADDRESS = 18
EXECUTABLE_WASM = 0
DURABILITY_PERSISTENT = 1
INSTANCE_KEY_XDR = struct.pack(">i", SCV_LEDGER_KEY_CONTRACT_INSTANCE)


# ── strkey / XDR ──────────────────────────────────────────────────────


def contract_id_bytes(contract_id: str) -> bytes:
    """32-byte hash of a C... strkey (version byte and CRC16 checked)."""
    raw = base64.b32decode(contract_id)
    if len(raw) != 35 or raw[0] != 2 << 3:
        raise ValueError(f"not a contract id: {contract_id}")
    body, checksum = raw[:-2], raw[-2:]
    if struct.pack("<H", crc16_xmodem(body)) != checksum:
        raise ValueError(f"bad checksum: {contract_id}")
    return body[1:]


def strkey(version: int, body: bytes) -> str:
    payload = bytes([version]) + body
    return base64.b32encode(payload + struct.pack("<H", crc16_xmodem(payload))).decode()


def crc16_xmodem(data: bytes) -> int:
    crc = 0
    for byte in data:
        crc ^= byte << 8
        for _ in range(8):
            crc = ((crc << 1) ^ 0x1021) if crc & 0x8000 else crc << 1
            crc &= 0xFFFF
    return crc


def data_ledger_key(contract_id: str, key_xdr: bytes) -> str:
    """Base64 LedgerKey for a persistent contract-data entry."""
    return base64.b64encode(
        struct.pack(">ii", LEDGER_ENTRY_CONTRACT_DATA, SC_ADDRESS_CONTRACT)
        + contract_id_bytes(contract_id)
        + key_xdr
        + struct.pack(">i", DURABILITY_PERSISTENT)
    ).decode()


def code_ledger_key(wasm_hash: bytes) -> str:
    return base64.b64encode(struct.pack(">i", LEDGER_ENTRY_CONTRACT_CODE) + wasm_hash).decode()


def wasm_hash_of(instance_entry_xdr: str) -> bytes | None:
    """The wasm hash in a contract instance's LedgerEntryData, or None for a
    Stellar Asset Contract. Layout: type, ext, ScAddress(type, 32 bytes), key
    (ScVal instance key), durability, val type, executable type, hash."""
    raw = base64.b64decode(instance_entry_xdr)
    (entry_type, _ext, addr_type) = struct.unpack_from(">iii", raw, 0)
    (key_type, _durability, val_type, exec_type) = struct.unpack_from(">iiii", raw, 44)
    if (entry_type, addr_type, key_type, val_type) != (
        LEDGER_ENTRY_CONTRACT_DATA,
        SC_ADDRESS_CONTRACT,
        SCV_LEDGER_KEY_CONTRACT_INSTANCE,
        SCV_CONTRACT_INSTANCE,
    ):
        raise ValueError("not a contract instance entry")
    return raw[60:92] if exec_type == EXECUTABLE_WASM else None


def describe_key(key_xdr: str) -> str:
    """A readable form of the enum-style keys these contracts use, e.g.
    'Job(83ca4422…)' or 'Rated(calculatorai, 72b0b685…)'. Falls back to the
    base64 XDR for anything else."""

    def scval(raw: bytes, at: int) -> tuple[str, int]:
        (kind,) = struct.unpack_from(">i", raw, at)
        at += 4
        if kind in (SCV_SYMBOL, SCV_BYTES):
            (n,) = struct.unpack_from(">I", raw, at)
            body = raw[at + 4 : at + 4 + n]
            at += 4 + n + (-n % 4)
            return (body.decode() if kind == SCV_SYMBOL else body.hex()[:16] + "…"), at
        if kind == SCV_ADDRESS:
            (addr_type,) = struct.unpack_from(">i", raw, at)
            # account: PublicKey(type, 32 bytes); contract: 32 bytes.
            size = 4 + 4 + 32 if addr_type == 0 else 4 + 32
            text = strkey(6 << 3 if addr_type == 0 else 2 << 3, raw[at + size - 32 : at + size])
            return f"{text[:5]}…{text[-4:]}", at + size
        if kind == SCV_VEC:
            (present, n) = struct.unpack_from(">iI", raw, at)
            at += 8
            items = []
            for _ in range(n if present else 0):
                item, at = scval(raw, at)
                items.append(item)
            head, *rest = items or ["?"]
            return f"{head}({', '.join(rest)})" if rest else head, at
        raise ValueError(kind)

    try:
        text, end = scval(base64.b64decode(key_xdr), 0)
        return text if end == len(base64.b64decode(key_xdr)) else key_xdr
    except (ValueError, struct.error, UnicodeDecodeError):
        return key_xdr


# ── network reads ─────────────────────────────────────────────────────


def http_json(url: str, body: dict | None = None) -> dict:
    data = json.dumps(body).encode() if body is not None else None
    headers = {"User-Agent": USER_AGENT, "Content-Type": "application/json"}
    for attempt in range(5):
        try:
            with urllib.request.urlopen(
                urllib.request.Request(url, data, headers), timeout=60
            ) as resp:
                return json.load(resp)
        except (urllib.error.URLError, TimeoutError) as err:
            if attempt == 4:
                raise SystemExit(f"✗ {url}: {err}") from err
            time.sleep(2 * (attempt + 1))
    raise AssertionError("unreachable")


class Rpc:
    def __init__(self, url: str):
        self.url = url

    def call(self, method: str, params: dict) -> dict:
        reply = http_json(self.url, {"jsonrpc": "2.0", "id": 1, "method": method, "params": params})
        if "error" in reply:
            raise SystemExit(f"✗ RPC {method}: {reply['error']}")
        return reply["result"]

    def latest(self) -> tuple[int, int]:
        seq = self.call("getLatestLedger", {})["sequence"]
        page = self.call("getLedgers", {"startLedger": seq, "pagination": {"limit": 1}})
        return seq, int(page["ledgers"][0]["ledgerCloseTime"])

    def entries(self, keys: list[str]) -> dict[str, dict]:
        """LedgerKey -> {xdr, liveUntilLedgerSeq} for every key the RPC knows."""
        found: dict[str, dict] = {}
        for i in range(0, len(keys), 200):
            result = self.call("getLedgerEntries", {"keys": keys[i : i + 200]})
            for entry in result.get("entries") or []:
                found[entry["key"]] = entry
        return found


def discover_keys(expert_network: str, contract_id: str) -> list[str]:
    """Base64 ScVal keys of a contract's persistent entries, from the
    stellar.expert contract-data index."""
    base = "https://api.stellar.expert"
    url = f"{base}/explorer/{expert_network}/contract-data/{contract_id}?order=asc&limit=200"
    keys: list[str] = []
    while url:
        page = http_json(url)
        records = page["_embedded"]["records"]
        keys += [r["key"] for r in records if r.get("durability") == "persistent"]
        url = base + page["_links"]["next"]["href"] if len(records) == 200 else ""
    return [k for k in keys if base64.b64decode(k) != INSTANCE_KEY_XDR]


# ── plan ──────────────────────────────────────────────────────────────


@dataclass
class Entry:
    field: str  # address-book field
    contract: str
    kind: str  # "instance" | "wasm" | "data"
    ledger_key: str
    key_xdr: str = ""  # ScVal, data entries only
    wasm_hash: str = ""  # hex, wasm only
    live_until: int | None = None  # None: not found; < now: archived


def collect(book: dict, rpc: Rpc, args, expert: str) -> list[Entry]:
    contracts = {
        f: v for f, v in book.items() if isinstance(v, str) and v.startswith("C") and len(v) == 56
    }
    instances = {
        f: Entry(f, c, "instance", data_ledger_key(c, INSTANCE_KEY_XDR)) for f, c in contracts.items()
    }
    entries: list[Entry] = list(instances.values())

    extra: dict[str, list[str]] = {}
    if args.keys_file:
        for line in Path(args.keys_file).read_text().splitlines():
            line = line.split("#", 1)[0].strip()
            if line:
                field, key = line.split()
                if field not in contracts:
                    raise SystemExit(f"✗ {args.keys_file}: no contract '{field}' in the address book")
                extra.setdefault(field, []).append(key)

    for field, contract in contracts.items():
        keys = list(extra.get(field, []))
        if args.discover and field not in INSTANCE_ONLY:
            keys += discover_keys(expert, contract)
        for key in dict.fromkeys(keys):
            entries.append(
                Entry(field, contract, "data", data_ledger_key(contract, base64.b64decode(key)), key)
            )

    found = rpc.entries([e.ledger_key for e in entries])
    for e in entries:
        if e.ledger_key in found:
            e.live_until = found[e.ledger_key].get("liveUntilLedgerSeq", 0)

    wasm: dict[bytes, Entry] = {}
    for e in instances.values():
        if e.ledger_key in found:
            h = wasm_hash_of(found[e.ledger_key]["xdr"])
            if h and h not in wasm:
                wasm[h] = Entry(e.field, e.contract, "wasm", code_ledger_key(h), wasm_hash=h.hex())
    codes = rpc.entries([w.ledger_key for w in wasm.values()])
    for w in wasm.values():
        if w.ledger_key in codes:
            w.live_until = codes[w.ledger_key].get("liveUntilLedgerSeq", 0)
    return entries + list(wasm.values())


def when(live_until: int | None, seq: int, close: int) -> str:
    if live_until is None:
        return "not found"
    if live_until < seq:
        return "ARCHIVED"
    t = dt.datetime.fromtimestamp(close + (live_until - seq) * LEDGER_SECONDS, dt.timezone.utc)
    return f"{live_until} (~{t:%Y-%m-%d %H:%M} UTC, {(live_until - seq) / DAY_IN_LEDGERS:.1f} d)"


def label(e: Entry) -> str:
    if e.kind == "wasm":
        return f"wasm {e.wasm_hash[:12]}…"
    if e.kind == "instance":
        return "instance"
    return describe_key(e.key_xdr)


def report(entries: list[Entry], seq: int, close: int, title: str) -> None:
    print(f"\n── {title} (ledger {seq}) ──")
    for field in dict.fromkeys(e.field for e in entries):
        group = [e for e in entries if e.field == field]
        print(f"{field}  {group[0].contract}")
        for e in sorted(group, key=lambda x: (x.kind != "instance", x.kind != "wasm", x.live_until or 0)):
            print(f"  {label(e):<70} {when(e.live_until, seq, close)}")


# ── commands ──────────────────────────────────────────────────────────


def commands(entries: list[Entry], seq: int, args, net: dict) -> tuple[list[list[str]], list[list[str]]]:
    """(restore commands, extend commands) for entries below the threshold."""
    common = [
        "--source-account", args.source or "<SOURCE>",
        "--rpc-url", args.rpc_url,
        "--network-passphrase", net["passphrase"],
    ]  # fmt: skip
    renew_below = args.renew_below_days * DAY_IN_LEDGERS
    archived = [e for e in entries if e.live_until is not None and e.live_until < seq]
    low = [e for e in entries if e.live_until and e.live_until >= seq and e.live_until - seq < renew_below]
    if args.restore_archived:
        low += archived

    def per_target(group: list[Entry], verb: list[str]) -> list[list[str]]:
        out: list[list[str]] = []
        for e in [e for e in group if e.kind == "instance"]:
            out.append(["stellar", "contract", *verb, "--id", e.contract, *common])
        for e in [e for e in group if e.kind == "wasm"]:
            out.append(["stellar", "contract", *verb, "--wasm-hash", e.wasm_hash, *common])
        data = [e for e in group if e.kind == "data"]
        for contract in dict.fromkeys(e.contract for e in data):
            keys = [e.key_xdr for e in data if e.contract == contract]
            for i in range(0, len(keys), BATCH):
                flags = [f for k in keys[i : i + BATCH] for f in ("--key-xdr", k)]
                out.append(
                    ["stellar", "contract", *verb, "--id", contract, "--durability", "persistent", *flags, *common]
                )
        return out

    restore = per_target(archived, ["restore"]) if args.restore_archived else []
    extend = per_target(low, ["extend", "--ledgers-to-extend", str(args.extend_to), "--ttl-ledger-only"])
    return restore, extend


def run(cmd: list[str]) -> None:
    shown = " ".join(shlex.quote(c) for c in cmd[:6]) + (" …" if len(cmd) > 6 else "")
    print(f"$ {shown}")
    result = subprocess.run(cmd, text=True, capture_output=True)
    if result.returncode != 0:
        print(result.stderr.strip(), file=sys.stderr)
        raise SystemExit(f"✗ command failed ({result.returncode})")
    if result.stdout.strip():
        print(f"  → {result.stdout.strip()}")


def main() -> None:
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument("--network", choices=NETWORKS, default="testnet")
    p.add_argument("--rpc-url", help="Soroban RPC (default: SDF testnet; required for mainnet)")
    p.add_argument("--book", help="address book (default: the network's addresses*.json)")
    p.add_argument("--apply", action="store_true", help="send the transactions (default: dry run)")
    p.add_argument("--source", help="stellar-cli identity that pays (required with --apply)")
    p.add_argument(
        "--extend-to",
        type=int,
        default=3_000_000,
        help="TTL in ledgers from now (default 3,000,000 ≈ 173 d; the network clamps to its maximum)",
    )
    p.add_argument("--renew-below-days", type=float, default=150, help="extend entries with less left")
    p.add_argument("--keys-file", help="extra '<field> <base64 ScVal key>' lines")
    p.add_argument("--no-discover", dest="discover", action="store_false", help="skip the stellar.expert index")
    p.add_argument("--restore-archived", action="store_true", help="restore archived entries, then extend")
    p.add_argument("--quiet", action="store_true", help="print only entries that need action")
    args = p.parse_args()

    net = NETWORKS[args.network]
    args.rpc_url = args.rpc_url or net["rpc"]
    if not args.rpc_url:
        raise SystemExit(f"✗ --rpc-url is required for {args.network}")
    if args.apply and not args.source:
        raise SystemExit("✗ --apply needs --source <funded stellar-cli identity>")
    book = json.loads((REPO / (args.book or net["book"])).read_text())
    if book.get("network") not in (None, args.network):
        raise SystemExit(f"✗ the address book is for {book.get('network')}, not {args.network}")

    rpc = Rpc(args.rpc_url)
    seq, close = rpc.latest()

    entries = collect(book, rpc, args, net["expert"])
    restore, extend = commands(entries, seq, args, net)
    shown = entries
    if args.quiet:
        renew_below = args.renew_below_days * DAY_IN_LEDGERS
        shown = [e for e in entries if not e.live_until or e.live_until - seq < renew_below]
    report(shown, seq, close, f"{args.network}: live-until before")

    archived = sum(1 for e in entries if e.live_until is not None and e.live_until < seq)
    missing = sum(1 for e in entries if e.live_until is None)
    print(
        f"\n{len(entries)} entries; {len(extend)} extend tx, "
        f"{len(restore)} restore tx; {archived} archived, {missing} not found"
    )
    if archived and not args.restore_archived:
        print("  archived entries are left as they are: add --restore-archived to bring them back")

    if not args.apply:
        print("\nDry run. These would be sent (add --apply --source <identity>):")
        for cmd in restore + extend:
            print(" ".join(shlex.quote(c) for c in cmd))
        return

    for cmd in restore + extend:
        run(cmd)
    seq, close = rpc.latest()
    after = collect(book, rpc, args, net["expert"])
    report(after, seq, close, f"{args.network}: live-until after")


if __name__ == "__main__":
    main()
