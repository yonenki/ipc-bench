#!/bin/bash
# IPC Transport ベンチマークスクリプト
#
# 使い方:
#   ./bench.sh              # 全シナリオ実行
#   ./bench.sh quick        # 高頻度小メッセージのみ
#   ./bench.sh large        # 大容量のみ
#   ./bench.sh vlarge       # 超大容量のみ

set -e

TRANSPORTS="shared_mem named_pipe unix_socket tcp_socket websocket"

run_scenario() {
    local label="$1"
    local size="$2"
    local count="$3"

    echo "========================================="
    echo " $label (size=${size}B x ${count})"
    echo "========================================="
    for t in $TRANSPORTS; do
        cargo run --release --bin ipc-server -- -t "$t" --count "$count" &
        sleep 0.3
        cargo run --release --bin ipc-client -- -t "$t" --count "$count" --size "$size" 2>&1
        wait
        echo ""
    done
}

mode="${1:-all}"

case "$mode" in
    quick)
        run_scenario "High Frequency" 64 100000
        ;;
    large)
        run_scenario "Large Payload (1MB)" 1048576 100
        ;;
    vlarge)
        run_scenario "Very Large Payload (10MB)" 10485760 20
        ;;
    all)
        run_scenario "High Frequency" 64 100000
        run_scenario "Large Payload (1MB)" 1048576 100
        run_scenario "Very Large Payload (10MB)" 10485760 20
        ;;
    *)
        echo "Usage: $0 [quick|large|vlarge|all]"
        exit 1
        ;;
esac
