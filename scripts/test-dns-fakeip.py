#!/usr/bin/env python3
"""fake-ip DNS 端到端测试。

验证三件事：
  1. 境内域名走 DIRECT，返回**真实 IP**；
  2. 境外域名走 PROXY，返回 `198.18.0.0/15` 的**假 IP**；
  3. 非 A 查询（AAAA）返回空答案，促使应用回退到 A。
"""

from __future__ import annotations

import socket
import struct
import sys

FAKEIP_FIRST = 198


def build_query(name: str, qtype: int = 1, tid: int = 0x1234) -> bytes:
    header = struct.pack("!HHHHHH", tid, 0x0100, 1, 0, 0, 0)
    qname = b"".join(bytes([len(p)]) + p.encode() for p in name.split("."))
    return header + qname + b"\x00" + struct.pack("!HH", qtype, 1)


def skip_name(data: bytes, offset: int) -> int:
    while offset < len(data):
        length = data[offset]
        if length == 0:
            return offset + 1
        if length & 0xC0 == 0xC0:
            return offset + 2
        offset += 1 + length
    raise ValueError("truncated name")


def parse_response(data: bytes) -> tuple[int, list[tuple[int, bytes]]]:
    """返回 (rcode, [(qtype, rdata)])。"""
    if len(data) < 12:
        raise ValueError("short response")
    _tid, flags, qd, an, _ns, _ar = struct.unpack("!HHHHHH", data[:12])
    rcode = flags & 0x0F
    offset = 12
    for _ in range(qd):
        offset = skip_name(data, offset) + 4
    answers: list[tuple[int, bytes]] = []
    for _ in range(an):
        offset = skip_name(data, offset)
        rtype, _rclass, _ttl, rdlength = struct.unpack("!HHIH", data[offset : offset + 10])
        offset += 10
        rdata = data[offset : offset + rdlength]
        offset += rdlength
        answers.append((rtype, rdata))
    return rcode, answers


def query(server: tuple[str, int], name: str, qtype: int = 1, timeout: float = 6.0):
    sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    sock.settimeout(timeout)
    try:
        sock.sendto(build_query(name, qtype), server)
        data, _ = sock.recvfrom(4096)
    finally:
        sock.close()
    return parse_response(data)


def main() -> int:
    endpoint = sys.argv[1] if len(sys.argv) > 1 else "127.0.0.1:5353"
    host, port_text = endpoint.rsplit(":", 1)
    server = (host, int(port_text))
    failures = 0

    # --- 1. 境内域名：DIRECT -> 真实 IP ---
    for name in ("www.baidu.com", "www.qq.com"):
        try:
            rcode, answers = query(server, name)
            addrs = [".".join(str(b) for b in rdata) for rtype, rdata in answers if rtype == 1]
            fake = [a for a in addrs if a.startswith(f"{FAKEIP_FIRST}.")]
            if rcode == 0 and addrs and not fake:
                print(f"DIRECT_OK {name:<18} -> {addrs[0]}")
            else:
                print(f"DIRECT_BAD {name:<18} -> rcode={rcode} addrs={addrs}")
                failures += 1
        except Exception as exc:  # noqa: BLE001 - 测试脚本
            print(f"DIRECT_FAIL {name} -> {exc}")
            failures += 1

    # --- 2. 境外域名：PROXY -> 假 IP ---
    fake_seen: list[str] = []
    for name in ("www.google.com", "www.youtube.com", "github.com"):
        try:
            rcode, answers = query(server, name)
            addrs = [".".join(str(b) for b in rdata) for rtype, rdata in answers if rtype == 1]
            if rcode == 0 and addrs and all(a.startswith(f"{FAKEIP_FIRST}.") for a in addrs):
                fake_seen.append(addrs[0])
                print(f"FAKEIP_OK {name:<18} -> {addrs[0]}")
            else:
                print(f"FAKEIP_BAD {name:<18} -> rcode={rcode} addrs={addrs}")
                failures += 1
        except Exception as exc:  # noqa: BLE001
            print(f"FAKEIP_FAIL {name} -> {exc}")
            failures += 1

    # --- 3. AAAA：空答案 ---
    try:
        rcode, answers = query(server, "www.google.com", qtype=28)
        if rcode == 0 and not answers:
            print("AAAA_OK  www.google.com   -> 0 answers (forces A fallback)")
        else:
            print(f"AAAA_BAD -> rcode={rcode} answers={len(answers)}")
            failures += 1
    except Exception as exc:  # noqa: BLE001
        print(f"AAAA_FAIL -> {exc}")
        failures += 1

    # --- 4. 同一域名重复查询应返回同一个假 IP（映射稳定）---
    if fake_seen:
        first = fake_seen[0]
        _rcode, answers = query(server, "www.google.com")
        again = [".".join(str(b) for b in rdata) for rtype, rdata in answers if rtype == 1]
        if again and again[0] == first:
            print(f"STABLE_OK www.google.com   -> {again[0]} (stable)")
        else:
            print(f"STABLE_BAD -> first={first} again={again}")
            failures += 1

    print(f"FAILURES={failures}")
    return 1 if failures else 0


if __name__ == "__main__":
    raise SystemExit(main())
