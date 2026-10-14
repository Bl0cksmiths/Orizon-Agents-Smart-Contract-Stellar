"""Characterisation tests for scripts/extend_ttl.py.

Nothing here touches a network or the stellar CLI: the guard fixture makes
any HTTP request or subprocess fail the test, and every read goes through a
fake RPC. Run from the repo root:

    python3 -m pip install pytest   # or: uv pip install pytest
    python3 -m pytest scripts/tests
"""

from __future__ import annotations

import argparse
import base64
import importlib.util
import struct
import sys
from pathlib import Path

import pytest

SCRIPT = Path(__file__).resolve().parent.parent / "extend_ttl.py"
_spec = importlib.util.spec_from_file_location("extend_ttl", SCRIPT)
ext = importlib.util.module_from_spec(_spec)
sys.modules["extend_ttl"] = ext
_spec.loader.exec_module(ext)


@pytest.fixture(autouse=True)
def no_network_or_cli(monkeypatch):
    """Fail loudly if anything tries to reach the network or run a command."""

    def refuse(*args, **kwargs):
        raise AssertionError(f"tests must not do I/O: {args!r}")

    monkeypatch.setattr(ext, "http_json", refuse)
    monkeypatch.setattr(ext.urllib.request, "urlopen", refuse)
    monkeypatch.setattr(ext.subprocess, "run", refuse)


# ── XDR builders ──────────────────────────────────────────────────────


def b64(raw: bytes) -> str:
    return base64.b64encode(raw).decode()


def opaque(kind: int, body: bytes) -> bytes:
    return struct.pack(">iI", kind, len(body)) + body + b"\0" * (-len(body) % 4)


def sym(text: str) -> bytes:
    return opaque(ext.SCV_SYMBOL, text.encode())


def scbytes(body: bytes) -> bytes:
    return opaque(ext.SCV_BYTES, body)


def account(key: bytes) -> bytes:
    return struct.pack(">iii", ext.SCV_ADDRESS, 0, 0) + key


def contract_addr(key: bytes) -> bytes:
    return struct.pack(">ii", ext.SCV_ADDRESS, 1) + key


def vec(*items: bytes) -> bytes:
    return struct.pack(">iiI", ext.SCV_VEC, 1, len(items)) + b"".join(items)


def contract_id(seed: int) -> str:
    return ext.strkey(2 << 3, bytes([seed]) * 32)


def instance_xdr(contract: str, wasm_hash: bytes | None) -> str:
    """LedgerEntryData of a contract instance: wasm executable, or a SAC."""
    exec_type = ext.EXECUTABLE_WASM if wasm_hash else 1
    return b64(
        struct.pack(">iii", ext.LEDGER_ENTRY_CONTRACT_DATA, 0, ext.SC_ADDRESS_CONTRACT)
        + ext.contract_id_bytes(contract)
        + struct.pack(
            ">iiii",
            ext.SCV_LEDGER_KEY_CONTRACT_INSTANCE,
            ext.DURABILITY_PERSISTENT,
            ext.SCV_CONTRACT_INSTANCE,
            exec_type,
        )
        + (wasm_hash or b"")
        + b"\0" * 8
    )


# ── describe_key ──────────────────────────────────────────────────────

JOB_ID = bytes.fromhex("83ca4422" + "ab" * 28)
RATED_ID = bytes.fromhex("72b0b685" + "cd" * 28)
KEY32 = bytes(range(32))


@pytest.mark.parametrize(
    ("raw", "expected"),
    [
        (sym("Counter"), "Counter"),
        (vec(sym("Admin")), "Admin"),
        (vec(sym("Job"), scbytes(JOB_ID)), "Job(83ca4422abababab…)"),
        (
            vec(sym("Rated"), sym("calculatorai"), scbytes(RATED_ID)),
            "Rated(calculatorai, 72b0b685cdcdcdcd…)",
        ),
        (scbytes(b"\x01\x02"), "0102…"),
        (vec(sym("A"), vec(sym("B"), sym("c"))), "A(B(c))"),
        (vec(), "?"),
        (struct.pack(">iiI", ext.SCV_VEC, 0, 0), "?"),
    ],
)
def test_describe_key_renders_enum_style_keys(raw, expected):
    assert ext.describe_key(b64(raw)) == expected


def test_describe_key_renders_account_and_contract_addresses():
    g = ext.strkey(6 << 3, KEY32)
    c = ext.strkey(2 << 3, KEY32)
    raw = vec(sym("Owner"), account(KEY32), contract_addr(KEY32))
    assert ext.describe_key(b64(raw)) == f"Owner({g[:5]}…{g[-4:]}, {c[:5]}…{c[-4:]})"


@pytest.mark.parametrize(
    "key_xdr",
    [
        b64(sym("Job") + b"\0\0\0\0"),  # trailing bytes
        b64(struct.pack(">iI", 3, 7)),  # an ScVal kind it doesn't render (u32)
        b64(struct.pack(">i", ext.SCV_SYMBOL)),  # truncated: struct.error
        b64(opaque(ext.SCV_SYMBOL, b"\xff\xfe")),  # not UTF-8
        "abc",  # not base64
        "",
    ],
)
def test_describe_key_falls_back_to_the_xdr(key_xdr):
    assert ext.describe_key(key_xdr) == key_xdr


# ── collect ───────────────────────────────────────────────────────────


class FakeRpc:
    """Answers getLedgerEntries from a dict and records every request."""

    def __init__(self, found: dict[str, dict], seq: int = 1_000, close: int = 1_700_000_000):
        self.found = found
        self.seq = seq
        self.close = close
        self.calls: list[list[str]] = []

    def latest(self) -> tuple[int, int]:
        return self.seq, self.close

    def entries(self, keys: list[str]) -> dict[str, dict]:
        self.calls.append(list(keys))
        return {k: self.found[k] for k in keys if k in self.found}


REG = contract_id(1)
LEDGER = contract_id(2)
SAC = contract_id(3)
WASM_A = bytes([0xAA]) * 32
KEY_JOB = b64(vec(sym("Job"), scbytes(JOB_ID)))
KEY_RATED = b64(vec(sym("Rated"), sym("calculatorai"), scbytes(RATED_ID)))
KEY_ADMIN = b64(vec(sym("Admin")))

BOOK = {
    "network": "testnet",
    "admin": "G" + "A" * 55,
    "agent_registry": REG,
    "short": "C" + "A" * 10,
    "count": 3,
    "reputation_ledger": LEDGER,
    "asset_sac": SAC,
}


def inst_key(contract: str) -> str:
    return ext.data_ledger_key(contract, ext.INSTANCE_KEY_XDR)


def data_key(contract: str, key: str) -> str:
    return ext.data_ledger_key(contract, base64.b64decode(key))


def args_for(keys_file=None, discover=True):
    return argparse.Namespace(keys_file=keys_file, discover=discover)


@pytest.fixture
def discovered(monkeypatch):
    calls: list[tuple[str, str]] = []
    index = {REG: [KEY_JOB, KEY_ADMIN], LEDGER: [KEY_RATED], SAC: ["should-not-be-asked"]}

    def fake(expert: str, contract: str) -> list[str]:
        calls.append((expert, contract))
        return list(index[contract])

    monkeypatch.setattr(ext, "discover_keys", fake)
    return calls


def test_collect_reads_instances_data_and_shared_wasm(discovered):
    rpc = FakeRpc(
        {
            inst_key(REG): {"xdr": instance_xdr(REG, WASM_A), "liveUntilLedgerSeq": 5_000},
            inst_key(LEDGER): {"xdr": instance_xdr(LEDGER, WASM_A), "liveUntilLedgerSeq": 6_000},
            inst_key(SAC): {"xdr": instance_xdr(SAC, None), "liveUntilLedgerSeq": 7_000},
            data_key(REG, KEY_JOB): {"xdr": "", "liveUntilLedgerSeq": 900},
            data_key(LEDGER, KEY_RATED): {"xdr": ""},  # no liveUntilLedgerSeq
            ext.code_ledger_key(WASM_A): {"xdr": "", "liveUntilLedgerSeq": 8_000},
        }
    )
    entries = ext.collect(BOOK, rpc, args_for(), "testnet")

    assert discovered == [("testnet", REG), ("testnet", LEDGER)]
    assert [(e.field, e.contract, e.kind, e.key_xdr, e.wasm_hash, e.live_until) for e in entries] == [
        ("agent_registry", REG, "instance", "", "", 5_000),
        ("reputation_ledger", LEDGER, "instance", "", "", 6_000),
        ("asset_sac", SAC, "instance", "", "", 7_000),
        ("agent_registry", REG, "data", KEY_JOB, "", 900),
        ("agent_registry", REG, "data", KEY_ADMIN, "", None),
        ("reputation_ledger", LEDGER, "data", KEY_RATED, "", 0),
        ("agent_registry", REG, "wasm", "", WASM_A.hex(), 8_000),
    ]
    assert [e.ledger_key for e in entries] == [
        inst_key(REG),
        inst_key(LEDGER),
        inst_key(SAC),
        data_key(REG, KEY_JOB),
        data_key(REG, KEY_ADMIN),
        data_key(LEDGER, KEY_RATED),
        ext.code_ledger_key(WASM_A),
    ]
    assert rpc.calls == [[e.ledger_key for e in entries[:-1]], [ext.code_ledger_key(WASM_A)]]


def test_collect_without_discovery_or_live_instances(discovered):
    rpc = FakeRpc({})
    entries = ext.collect(BOOK, rpc, args_for(discover=False), "public")

    assert discovered == []
    assert [(e.field, e.kind, e.live_until) for e in entries] == [
        ("agent_registry", "instance", None),
        ("reputation_ledger", "instance", None),
        ("asset_sac", "instance", None),
    ]
    assert rpc.calls == [[inst_key(REG), inst_key(LEDGER), inst_key(SAC)], []]


def test_collect_merges_the_keys_file_before_discovery(discovered, tmp_path):
    keys_file = tmp_path / "keys.txt"
    keys_file.write_text(
        "# hand-listed keys\n"
        "\n"
        f"agent_registry {KEY_ADMIN}   # also discovered\n"
        f"  asset_sac {KEY_RATED}\n"
        f"agent_registry {KEY_RATED}\n"
    )
    entries = ext.collect(BOOK, FakeRpc({}), args_for(keys_file=str(keys_file)), "testnet")

    assert [(e.field, e.kind, e.key_xdr) for e in entries if e.kind == "data"] == [
        ("agent_registry", "data", KEY_ADMIN),
        ("agent_registry", "data", KEY_RATED),
        ("agent_registry", "data", KEY_JOB),
        ("reputation_ledger", "data", KEY_RATED),
        ("asset_sac", "data", KEY_RATED),
    ]


def test_collect_rejects_a_keys_file_field_not_in_the_book(discovered, tmp_path):
    keys_file = tmp_path / "keys.txt"
    keys_file.write_text(f"agent_registry {KEY_JOB}\nescrow {KEY_JOB}\n")
    with pytest.raises(SystemExit) as exc:
        ext.collect(BOOK, FakeRpc({}), args_for(keys_file=str(keys_file)), "testnet")
    assert str(exc.value) == f"✗ {keys_file}: no contract 'escrow' in the address book"
    assert discovered == []


def test_collect_rejects_a_malformed_keys_file_line(discovered, tmp_path):
    keys_file = tmp_path / "keys.txt"
    keys_file.write_text("agent_registry\n")
    with pytest.raises(ValueError, match="not enough values to unpack"):
        ext.collect(BOOK, FakeRpc({}), args_for(keys_file=str(keys_file)), "testnet")
