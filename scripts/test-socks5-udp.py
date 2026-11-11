#!/usr/bin/env python3
"""SOCKS5 UDP ASSOCIATE 端到端测试。

复用 client-core 的 SOCKS5 入站，验证两条路径：
  - PROXY  : 境外目标（走 VLESS UDP，command=2）
  - DIRECT : 境内目标（走系统 UDP socket）
"""

from __future__ import annotations

import os
import socket
import struct
import sys


def socks5_udp(
    proxy: tuple[str, int],
    dst_addr: str,
    dst_port: int,
    payload: bytes,
    timeout: float = 8.0,
) -> bytes:
    """完成 UDP ASSOCIATE 并收发一个数据报，返回响应载荷。"""
    ctrl = socket.create_connection(proxy, timeout=timeout)

    ctrl.sendall(b"\x05\x01\x00")
    greeting = ctrl.recv(2)
    if greeting != b"\x05\x00":
        raise RuntimeError(f"greeting failed: {greeting!r}")

    # VER CMD=3 RSV ATYP=IPv4 0.0.0.0:0
    ctrl.sendall(b"\x05\x03\x00\x01" + b"\x00" * 4 + b"\x00\x00")
    reply = ctrl.recv(10)
    if len(reply) < 10 or reply[1] != 0:
        raise RuntimeError(f"associate failed: {reply!r}")

    bound_ip = socket.inet_ntoa(reply[4:8])
    bound_port = struct.unpack("!H", reply[8:10])[0]

    try:
        addr_field = b"\x01" + socket.inet_aton(dst_addr)
    except OSError:
        encoded = dst_addr.encode()
        addr_field = b"\x03" + bytes([len(encoded)]) + encoded

    packet = b"\x00\x00\x00" + addr_field + struct.pack("!H", dst_port) + payload

    udp = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    udp.settimeout(timeout)
    try:
        udp.sendto(packet, (bound_ip, bound_port))
        data, _ = udp.recvfrom(65535)
    finally:
        udp.close()
        ctrl.close()

    atyp = data[3]
    if atyp == 1:
        offset = 8
    elif atyp == 3:
        offset = 5 + data[4]
    elif atyp == 4:
        offset = 20
    else:
        raise RuntimeError(f"unexpected reply atyp {atyp}")
    offset += 2
    return data[offset:]


def dns_query(name: str) -> bytes:
    # DNS 头固定 12 字节：ID(2) FLAGS(2) QDCOUNT(2) ANCOUNT(2) NSCOUNT(2) ARCOUNT(2)
    header = b"\xab\xcd" + b"\x01\x00" + b"\x00\x01" + b"\x00\x00" * 3
    qname = b"".join(bytes([len(part)]) + part.encode() for part in name.split("."))
    return header + qname + b"\x00" + struct.pack("!HH", 1, 1)


def main() -> int:
    listen = sys.argv[1] if len(sys.argv) > 1 else "127.0.0.1:1080"
    host, port_text = listen.rsplit(":", 1)
    proxy = (host, int(port_text))
    failures = 0

    # --- 1. PROXY 路径：境外 UDP echo（VLESS command=2）---
    echo_target = (
        os.environ.get("ECHO_HOST", "127.0.0.1"),
        int(os.environ.get("ECHO_PORT", "30081")),
    )
    payload = b"udp-probe-proxy"
    try:
        response = socks5_udp(proxy, *echo_target, payload)
        if response == b"UDP-ECHO:" + payload:
            print(f"PROXY_OK  {echo_target[0]}:{echo_target[1]} -> {response!r}")
        else:
            print(f"PROXY_BAD {echo_target} -> {response!r}")
            failures += 1
    except Exception as exc:  # noqa: BLE001 - 测试脚本需要打印任意异常
        print(f"PROXY_FAIL {echo_target} -> {exc}")
        failures += 1

    # --- 2. DIRECT 路径：境内 DNS（GEOIP,CN -> DIRECT）---
    dns_target = ("223.5.5.5", 53)
    query = dns_query("www.baidu.com")
    try:
        response = socks5_udp(proxy, *dns_target, query)
        if len(response) > 12 and response[:2] == b"\xab\xcd":
            print(f"DIRECT_OK {dns_target[0]}:{dns_target[1]} -> {len(response)} bytes, "
                  f"answers={struct.unpack('!H', response[6:8])[0]}")
        else:
            print(f"DIRECT_BAD {dns_target} -> {len(response)} bytes")
            failures += 1
    except Exception as exc:  # noqa: BLE001
        print(f"DIRECT_FAIL {dns_target} -> {exc}")
        failures += 1

    # --- 3. 对照：同一 SOCKS5 入站的 TCP 仍要正常（回归）---
    try:
        sock = socket.create_connection(proxy, timeout=8)
        sock.sendall(b"\x05\x01\x00")
        sock.recv(2)
        sock.close()
        print("TCP_HANDSHAKE_OK")
    except Exception as exc:  # noqa: BLE001
        print(f"TCP_HANDSHAKE_FAIL -> {exc}")
        failures += 1

    print(f"FAILURES={failures}")
    return 1 if failures else 0


if __name__ == "__main__":
    raise SystemExit(main())
