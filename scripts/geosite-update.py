#!/usr/bin/env python3
"""生成 `geosite-cn.txt`（client-core 的 `--geosite` 输入）。

v2fly 的 `data/cn` 只是一个 **include 汇总文件**，真正的规则散落在上百个
子分类（`tld-cn`、`geolocation-cn`、`baidu`、`tencent`…）里。之前逐个
HTTP 抓取会因请求过多而超时，这里改为**一次下载仓库 tarball**，在本地
内存中递归展开，全程零第三方依赖。

输出保持 v2fly 文本格式（`domain:` / `full:` / `keyword:` / 裸域名），
`GeoSite::parse` 可直接读取。

关于 `regexp:`：Rust 侧没有正则引擎，这类规则会被跳过。脚本默认丢弃并
在头部注明丢弃条数，避免"看着有规则、实际没生效"。

用法::

    python3 script/geosite-update.py                    # stdout
    python3 script/geosite-update.py -o geo/geosite-cn.txt
    python3 script/geosite-update.py --category geolocation-cn --stats
"""

from __future__ import annotations

import argparse
import io
import re
import sys
import tarfile
import time
import urllib.request

TARBALL_URL = (
    "https://codeload.github.com/v2fly/domain-list-community/tar.gz/refs/heads/master"
)

# `domain:example.com @cn` -> 去掉行尾的 `@attribute` 标注。
ATTRIBUTE_RE = re.compile(r"\s+@[A-Za-z0-9_-]+.*$")
# `include:xxx@cn` / `include:xxx` -> 取分类名。
INCLUDE_RE = re.compile(r"^include:([^@\s]+)")

MAX_DEPTH = 8


def fetch_tarball(url: str, timeout: int = 300) -> tarfile.TarFile:
    request = urllib.request.Request(url, headers={"User-Agent": "geosite-update/1.0"})
    with urllib.request.urlopen(request, timeout=timeout) as response:
        payload = response.read()
    print(f"tarball {len(payload) / 1024 / 1024:.1f} MiB", file=sys.stderr)
    return tarfile.open(fileobj=io.BytesIO(payload), mode="r:gz")


def index_categories(archive: tarfile.TarFile) -> dict[str, str]:
    """把 tarball 里的 `*/data/<name>` 读成 `{分类名: 文本内容}`。"""
    categories: dict[str, str] = {}
    for member in archive.getmembers():
        if not member.isfile():
            continue
        parts = member.name.split("/")
        # 形如 domain-list-community-master/data/cn
        if len(parts) < 3 or parts[1] != "data":
            continue
        name = "/".join(parts[2:])
        handle = archive.extractfile(member)
        if handle is None:
            continue
        categories[name] = handle.read().decode("utf-8", "replace")
    return categories


class Expander:
    """递归展开 include，收集实际规则。"""

    def __init__(self, categories: dict[str, str]) -> None:
        self.categories = categories
        self.rules: list[str] = []
        self.visited: list[str] = []
        self.missing: list[str] = []
        self.regexp_dropped = 0
        self._seen: set[str] = set()
        self._emitted: set[str] = set()

    def run(self, root: str) -> None:
        self._expand(root, 0)

    def _expand(self, name: str, depth: int) -> None:
        if depth > MAX_DEPTH or name in self._seen:
            return
        self._seen.add(name)
        text = self.categories.get(name)
        if text is None:
            self.missing.append(name)
            return
        self.visited.append(name)

        for raw in text.splitlines():
            line = raw.split("#")[0].strip()
            if not line:
                continue
            line = ATTRIBUTE_RE.sub("", line).strip()
            if not line:
                continue

            include_match = INCLUDE_RE.match(line)
            if include_match:
                self._expand(include_match.group(1), depth + 1)
                continue
            if line.startswith("regexp:"):
                self.regexp_dropped += 1
                continue
            self._emit(line)

    def _emit(self, line: str) -> None:
        # 统一小写去重；`full:` / `domain:` / `keyword:` 前缀保留。
        key = line.lower()
        if key in self._emitted:
            return
        self._emitted.add(key)
        self.rules.append(key)


def main() -> int:
    parser = argparse.ArgumentParser(description="生成 geosite-cn.txt")
    parser.add_argument("-o", "--out", help="输出文件（默认 stdout）")
    parser.add_argument("--category", default="cn", help="根分类名，默认 cn")
    parser.add_argument(
        "--no-timestamp", action="store_true", help="不写生成时间（可复现）"
    )
    parser.add_argument("--stats", action="store_true", help="打印展开统计到 stderr")
    args = parser.parse_args()

    archive = fetch_tarball(TARBALL_URL)
    categories = index_categories(archive)
    if args.category not in categories:
        print(
            f"category '{args.category}' not found in tarball "
            f"(available: {len(categories)})",
            file=sys.stderr,
        )
        return 1

    expander = Expander(categories)
    expander.run(args.category)

    header = [
        "# geosite-cn: CN domain list for client-core --geosite",
        f"# root: {args.category} expanded via include (depth<={MAX_DEPTH})",
        "# source: github.com/v2fly/domain-list-community (master tarball)",
        f"# categories: {len(expander.visited)} files merged",
        f"# entries: {len(expander.rules)} rules"
        + (f" (regexp dropped: {expander.regexp_dropped})" if expander.regexp_dropped else ""),
    ]
    if not args.no_timestamp:
        header.append(
            f"# generated: {time.strftime('%Y-%m-%dT%H:%M:%SZ', time.gmtime())}"
        )

    output = "\n".join(header) + "\n" + "\n".join(expander.rules) + "\n"

    if args.out:
        with open(args.out, "w", encoding="utf-8") as handle:
            handle.write(output)
        print(
            f"wrote {args.out}: {len(expander.rules)} rules from "
            f"{len(expander.visited)} categories",
            file=sys.stderr,
        )
    else:
        sys.stdout.write(output)

    if args.stats:
        print(
            f"categories={len(expander.visited)} rules={len(expander.rules)} "
            f"regexp_dropped={expander.regexp_dropped} "
            f"missing={len(expander.missing)}",
            file=sys.stderr,
        )
        if expander.missing:
            preview = ", ".join(expander.missing[:10])
            print(f"missing includes: {preview}", file=sys.stderr)

    return 0


if __name__ == "__main__":
    raise SystemExit(main())
