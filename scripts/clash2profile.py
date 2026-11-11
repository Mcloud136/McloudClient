#!/usr/bin/env python3
"""Convert a Clash/Mihomo YAML subscription into client-core's JSON profile.

Kept: ``proxies``, ``proxy-groups``, ``rule-providers``, ``rules``.
Unknown fields are ignored by the Rust side.

``rule-providers`` 处理方式：
- 若 ``path`` 指向本地缓存文件（Clash 自己下载过），直接读取；
- 否则用 ``url`` 下载；
- 把 ``payload`` 列表写成**纯文本**（每行一条），交给 Rust 侧按
  ``behavior`` 解析（Rust 不引入 YAML 依赖）。
- 无法获取的 provider 会被跳过，并**同步丢弃引用它的规则**（与 Clash
  的降级语义一致），同时打印警告。

Usage:
    python3 script/clash2profile.py subscription.yaml -o profile.json
    python3 script/clash2profile.py sub.yaml -o out/profile.json --rules-dir out/rules
"""

from __future__ import annotations

import argparse
import json
import sys
import urllib.request
from pathlib import Path


def parse_payload(text: str) -> list[str]:
    import yaml

    data = yaml.safe_load(text) or {}
    payload = data.get("payload") or []
    return [str(item).strip() for item in payload if str(item).strip()]


def load_provider_payload(spec: dict, base_dir: Path) -> list[str] | None:
    """优先读本地缓存（Clash 的 `path`），否则按 `url` 下载。"""
    local = spec.get("path")
    if local:
        candidate = Path(local)
        if not candidate.is_absolute():
            candidate = base_dir / candidate
        if candidate.is_file():
            try:
                return parse_payload(candidate.read_text(encoding="utf-8"))
            except Exception as exc:  # noqa: BLE001 - 需要报告任意解析错误
                print(f"  local {candidate} unreadable: {exc}", file=sys.stderr)

    url = spec.get("url")
    if url:
        try:
            request = urllib.request.Request(
                url, headers={"User-Agent": "clash2profile/1.0"}
            )
            with urllib.request.urlopen(request, timeout=60) as response:
                return parse_payload(response.read().decode("utf-8", "replace"))
        except Exception as exc:  # noqa: BLE001
            print(f"  download {url} failed: {exc}", file=sys.stderr)
    return None


def convert_providers(
    providers: dict, base_dir: Path, rules_dir: Path
) -> tuple[dict, set[str]]:
    """把 Clash 的 rule-providers 转成 client-core 形态（本地纯文本）。"""
    converted: dict[str, dict] = {}
    skipped: set[str] = set()
    for name, spec in (providers or {}).items():
        spec = spec or {}
        behavior = spec.get("behavior") or "domain"
        payload = load_provider_payload(spec, base_dir)
        if not payload:
            print(f"  provider {name}: no payload, skipped", file=sys.stderr)
            skipped.add(name)
            continue
        out_path = rules_dir / f"{name}.txt"
        out_path.write_text("\n".join(payload) + "\n", encoding="utf-8")
        converted[name] = {"behavior": behavior, "path": str(out_path)}
        print(f"  provider {name}: {len(payload)} entries ({behavior}) -> {out_path}")
    return converted, skipped


def drop_rules_for_skipped(rules: list[str], skipped: set[str]) -> tuple[list[str], int]:
    if not skipped:
        return rules, 0
    kept: list[str] = []
    dropped = 0
    for rule in rules:
        parts = [part.strip() for part in str(rule).split(",")]
        if len(parts) >= 2 and parts[0].upper() == "RULE-SET" and parts[1] in skipped:
            dropped += 1
            continue
        kept.append(rule)
    return kept, dropped


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("input", help="Clash YAML file")
    parser.add_argument("-o", "--output", default="profile.json")
    parser.add_argument(
        "--rules-dir",
        help="生成规则集文本的目录（默认与输出同级的 rules/）",
    )
    args = parser.parse_args()

    try:
        import yaml
    except ImportError:
        print("PyYAML is required: pip install pyyaml", file=sys.stderr)
        return 1

    source = Path(args.input)
    with source.open(encoding="utf-8") as handle:
        data = yaml.safe_load(handle) or {}

    output = Path(args.output)
    rules_dir = Path(args.rules_dir) if args.rules_dir else output.parent / "rules"
    rules_dir.mkdir(parents=True, exist_ok=True)

    print("converting rule-providers:", file=sys.stderr)
    providers, skipped = convert_providers(
        data.get("rule-providers") or {}, source.resolve().parent, rules_dir
    )

    rules = data.get("rules") or []
    rules, dropped = drop_rules_for_skipped(rules, skipped)
    if dropped:
        print(
            f"dropped {dropped} rules referencing skipped providers",
            file=sys.stderr,
        )

    profile = {
        "proxies": data.get("proxies") or [],
        "proxy-groups": data.get("proxy-groups") or [],
        "rule-providers": providers,
        "rules": rules,
    }
    with output.open("w", encoding="utf-8") as handle:
        json.dump(profile, handle, ensure_ascii=False, indent=2)

    print(
        f"wrote {output}: {len(profile['proxies'])} proxies, "
        f"{len(profile['proxy-groups'])} groups, "
        f"{len(providers)} rule-providers, {len(profile['rules'])} rules"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
