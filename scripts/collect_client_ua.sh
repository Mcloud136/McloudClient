#!/usr/bin/env bash
# C0a：客户端类型 / 版本分布只读采集。
#
# 用途：为 G1 立项门控提供「目标用户客户端版本分布」数据。
# 依据：docs/client-design-plan.md §6、docs/benchmark-spec.md、docs/g1-gate-review.md。
#
# 必须在【面板机】执行。注意：节点机与内网推理机不是面板机，
# 这两台上没有 /www/wwwlogs 与 /www/wwwroot，不能用于采集。
#
# 用法：
#   bash collect_client_ua.sh                         # 自动探测日志 + 生产订阅路径
#   bash collect_client_ua.sh <access_log>            # 指定单日志，使用默认订阅路径
#   bash collect_client_ua.sh <log1> <log2> ...       # 多个日志文件，订阅路径取默认
#   bash collect_client_ua.sh <access_log> <sub_path> # 单日志 + 显式订阅路径
#   bash collect_client_ua.sh -                      # 从 stdin 读（仅在面板机本地管道使用）
#   bash collect_client_ua.sh --help
#
# 生产事实（来自 v2board 源码与部署记录，不是猜测）：
#   - 订阅路径为自定义 /Delta/Force/BigRED（v2board.subscribe_path）；
#     源码默认值才是 /api/v1/client/subscribe（app/Utils/Helper.php:109）。
#   - 面板根目录 /www/wwwroot/example.com（按实际部署替换）；
#   - access log 路径族 /www/wwwlogs/{www.example.com,example.com,pay.example.com}.log。
#
# 原理：v2board 的 ClientController::subscribe 用 `?flag=` 或 User-Agent
# 匹配 app/Protocols/*::$flag 决定订阅格式，因此 access log 天然携带客户端类型。
#
# 安全约束（必须遵守）：
#   - 只输出聚合计数，绝不输出整行、token、用户 ID 或 IP；
#   - 不写库、不改面板、不改中间件、不把原始日志拷贝出面板机；
#   - 假设 nginx combined 日志格式（UA 为第 6 个引号分隔字段）。
# 订阅 URL 含 `token=` 明文，历史日志已有残留先例，切勿 cat 原始日志。
#
# 若不便提供 SSH：在面板机执行本脚本后，把【聚合输出】贴回即可，无需传日志本身。

set -euo pipefail

SUB_DEFAULT='/Delta/Force/BigRED'
LOG_CANDIDATES=(
    /www/wwwlogs/www.example.com.log
    /www/wwwlogs/example.com.log
    /www/wwwlogs/pay.example.com.log
)

usage() {
    cat >&2 <<EOF
usage: bash collect_client_ua.sh [access_log_path|-] [subscribe_path]

  access_log_path  nginx combined 格式日志；省略则自动探测：
                   ${LOG_CANDIDATES[*]}
                   '-' 表示从 stdin 读（仅限面板机本地管道）
  subscribe_path   v2board.subscribe_path，默认 ${SUB_DEFAULT}
  -h / --help      显示本说明

只输出聚合计数，不输出原始日志行 / token / IP。
EOF
}

case "${1:-}" in
    -h|--help)
        usage
        exit 0
        ;;
esac

# 参数解析：位置参数中「以 / 开头且存在」的视为日志文件，其余视为订阅路径。
# 这样既支持单日志、也支持多日志，且不破坏「单日志 + 显式订阅路径」的旧用法。
SUB_PATH="$SUB_DEFAULT"
RAW_LOGS=()
for arg in "$@"; do
    if [[ "$arg" == "auto" || "$arg" == "-" ]]; then
        RAW_LOGS+=("$arg")
    elif [[ "$arg" == /* && -r "$arg" ]]; then
        RAW_LOGS+=("$arg")
    else
        SUB_PATH="$arg"
    fi
done

LOGS=()
if [[ ${#RAW_LOGS[@]} -eq 0 ]]; then
    for candidate in "${LOG_CANDIDATES[@]}"; do
        [[ -r "$candidate" ]] && LOGS+=("$candidate")
    done
elif [[ "${RAW_LOGS[0]}" == "auto" ]]; then
    for candidate in "${LOG_CANDIDATES[@]}"; do
        [[ -r "$candidate" ]] && LOGS+=("$candidate")
    done
elif [[ "${RAW_LOGS[0]}" == "-" ]]; then
    LOGS=("-")
else
    LOGS=("${RAW_LOGS[@]}")
fi

if (( ${#LOGS[@]} == 0 )); then
    echo "ERROR: no readable access log under /www/wwwlogs/; pass a path explicitly." >&2
    echo "       candidates: ${LOG_CANDIDATES[*]}" >&2
    exit 2
fi

# 统一数据源：多文件用 -h 抑制文件名前缀；无匹配时返回空而不中断脚本。
feed() {
    if (( ${#LOGS[@]} == 1 )) && [[ "${LOGS[0]}" == "-" ]]; then
        grep -hF -- "$SUB_PATH" - || true
    else
        grep -hF -- "$SUB_PATH" "${LOGS[@]}" || true
    fi
}

echo "C0a client distribution (read-only aggregate)"
echo "logs=${LOGS[*]}"
echo "subscribe_path=$SUB_PATH  (v2board.subscribe_path)"
echo "note: UA is the 6th quote-delimited field of nginx combined format"
echo "------------------------------------------------------------"

echo "=== 1) User-Agent distribution (top 50) ==="
feed \
    | awk -F'"' '{ ua = $6; if (ua == "" || ua == "-") ua = "(empty)"; print ua }' \
    | sort | uniq -c | sort -rn | head -50

echo "=== 2) explicit ?flag= distribution ==="
feed \
    | grep -oE '[?&]flag=[A-Za-z0-9._%-]+' \
    | sort | uniq -c | sort -rn || true

echo "=== 3) category aggregation ==="
feed | awk -F'"' '
    {
        ua  = $6
        req = $2
        total++
        flag = ""
        if (match(req, /[?&]flag=[^&" ]+/)) {
            flag = substr(req, RSTART, RLENGTH)
            sub(/^[?&]flag=/, "", flag)
        }
        key = (flag != "") ? tolower(flag) : tolower(ua)
        if (key ~ /sing/)                                     cat["sing-box"]++
        else if (key ~ /meta|clash|verge|nyanpasu/)           cat["Clash_Mihomo"]++
        else if (key ~ /v2ray|xray|v2raytun/)                 cat["Xray"]++
        else if (key ~ /shadowrocket|loon|stash|surge|quantumult|surfboard|passwall|sagernet|ssrplus|shadowsocks/)
                                                              cat["other"]++
        else                                                  cat["unidentified"]++
    }
    END {
        order[1]="Clash_Mihomo"; order[2]="sing-box"; order[3]="Xray"
        order[4]="other";       order[5]="unidentified"
        for (i=1; i<=5; i++) {
            k = order[i]; n = cat[k] + 0
            pct = (total > 0) ? sprintf("%.1f", 100*n/total) : "0.0"
            printf "%-16s %8d  %6s%%\n", k, n, pct
        }
        identified = total - cat["unidentified"]
        rate = (total > 0) ? sprintf("%.1f", 100*identified/total) : "0.0"
        printf "%-16s %8d\n", "total", total
        printf "%-16s %8s%%\n", "identifiable", rate
    }
'

echo "------------------------------------------------------------"
echo "Next: 归档聚合结果到 docs/benchmark-results/，统计 Windows 客户端占比上限"
echo "      与最低支持版本；可识别率过低时改用小样本问卷，不要为此改中间件。"
