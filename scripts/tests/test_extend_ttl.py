"""Characterisation tests for scripts/extend_ttl.py.

Nothing here touches a network or the stellar CLI: the guard fixture makes
any HTTP request or subprocess fail the test, and every read goes through a
fake RPC. Run from the repo root:

    python3 -m pip install pytest   # or: uv pip install pytest
    python3 -m pytest scripts/tests
"""

from __future__ import annotations

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
