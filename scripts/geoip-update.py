#!/usr/bin/env python3
"""生成 `geoip-cn.txt`（client-core 的 `--geoip` 输入）。

只用标准库，无第三方依赖。数据源：

1. APNIC `delegated-apnic-latest`
   官方注册数据，权威、可审计。每天更新。
   语义：**已分配给 CN 的地址段**（不代表一定在境内路由）。

2. misakaio/chnroutes2 `chnroutes.txt`
   从多个 BGP feed 采集，每小时更新。CC-BY-SA 2.0。
   语义：**当前实际在 CN 路由表中宣告的地址段**。

默认取并集：注册段 + 实际路由段。理由：
- 漏判（国内站走代理）体验损失大；多判（境外段走直连）只是连接失败，
  而这类段本来也无服务，代价可忽略。
- `--sources chnroutes2` 可切到更严格的 BGP-only 口径。

用法::

    python3 script/geoip-update.py                    # 输出到 stdout
    python3 script/geoip-update.py -o geo/geoip-cn.txt
    python3 script/geoip-update.py --sources chnroutes2 -o geo/geoip-cn.txt
"""

from __future__ import annotations

import argparse
import ipaddress
import sys
import time
import urllib.request

APNIC_URL = "https://ftp.apnic.net/stats/apnic/delegated-apnic-latest"
CHNROUTES_URL = (
    "https://raw.githubusercontent.com/misakaio/chnroutes2/master/chnroutes.txt"
)

Network = ipaddress.IPv4Network


def fetch(url: str, timeout: int = 120) -> str:
    request = urllib.request.Request(url, headers={"User-Agent": "geoip-update/1.0"})
    with urllib.request.urlopen(request, timeout=timeout) as response:
        return response.read().decode("utf-8", "replace")


def load_apnic() -> list[Network]:
    """APNIC delegated 文件里 `CN|ipv4|allocated` 的记录。

    文件用 `|` 分隔，第 5 列是**地址个数**而非前缀长度，
    需要换算成 CIDR 前缀（个数必为 2 的幂）。
    """
    raw = fetch(APNIC_URL)
    nets: list[Network] = []
    for line in raw.splitlines():
        parts = line.strip().split("|")
        if len(parts) < 7:
            continue
        registry, country, kind, start, count, _date, status = parts[:7]
        if kind != "ipv4" or country != "CN" or status != "allocated":
            continue
        size = int(count)
        if size <= 0 or size & (size - 1):
            # 非 2 的幂，无法表达为单个 CIDR，跳过（APNIC 实际不会出现）。
            continue
        nets.append(
            ipaddress.ip_network(f"{start}/{32 - (size.bit_length() - 1)}", strict=False)
        )
    return nets


def load_chnroutes2() -> list[Network]:
    """chnroutes2 的纯 CIDR 列表，`#` 为注释。"""
    raw = fetch(CHNROUTES_URL)
    nets: list[Network] = []
    for line in raw.splitlines():
        line = line.strip()
        if not line or line.startswith("#"):
            continue
        # 兼容 bird 风格 `route 1.0.1.0/24` 与裸 CIDR 两种写法。
        if line.startswith("route"):
            line = line.split(None, 1)[-1].strip()
        try:
            nets.append(ipaddress.ip_network(line, strict=False))
        except ValueError:
            continue
    return nets


def collapse(nets: list[Network]) -> list[Network]:
    return list(ipaddress.collapse_addresses(nets))


def main() -> int:
    parser = argparse.ArgumentParser(description="生成 geoip-cn.txt")
    parser.add_argument("-o", "--out", help="输出文件（默认 stdout）")
    parser.add_argument(
        "--sources",
        choices=("both", "apnic", "chnroutes2"),
        default="both",
        help="数据源组合，默认 both（并集）",
    )
    parser.add_argument(
        "--no-timestamp", action="store_true", help="输出不写生成时间（可复现构建）"
    )
    args = parser.parse_args()

    collected: list[Network] = []
    summary: list[str] = []

    if args.sources in ("both", "apnic"):
        apnic = collapse(load_apnic())
        collected += apnic
        summary.append(f"apnic={len(apnic)}")
    if args.sources in ("both", "chnroutes2"):
        bgroutes = collapse(load_chnroutes2())
        collected += bgroutes
        summary.append(f"chnroutes2={len(bgroutes)}")

    merged = collapse(collected)
    addresses = sum(net.num_addresses for net in merged)

    header = [
        "# geoip-cn: CN IPv4 CIDR list for client-core --geoip",
        f"# sources: {args.sources} ({' '.join(summary)})",
        "# apnic: ftp.apnic.net/stats/apnic/delegated-apnic-latest (daily)",
        "# chnroutes2: github.com/misakaio/chnroutes2 (hourly, CC-BY-SA 2.0)",
    ]
    if not args.no_timestamp:
        header.append(f"# generated: {time.strftime('%Y-%m-%dT%H:%M:%SZ', time.gmtime())}")
    header.append(f"# entries: {len(merged)} cidrs, {addresses} addresses")

    body = "\n".join(str(net) for net in merged)
    output = "\n".join(header) + "\n" + body + "\n"

    if args.out:
        with open(args.out, "w", encoding="utf-8") as handle:
            handle.write(output)
        print(
            f"wrote {args.out}: {len(merged)} cidrs, {addresses:,} addresses",
            file=sys.stderr,
        )
    else:
        sys.stdout.write(output)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
