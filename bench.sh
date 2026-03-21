#!/bin/bash
# IPC Transport ベンチマークスクリプト
#
# 使い方:
#   ./bench.sh              # 全シナリオ実行
#   ./bench.sh quick        # 高頻度小メッセージのみ
#   ./bench.sh large        # 大容量のみ
#   ./bench.sh vlarge       # 超大容量のみ

set -e

# SharedMem 検証マトリクス (2軸 × 2値 = 4パターン):
#   HeaderLayout: padded (cache line分離) / compact (false sharing)
#   FrameStrategy: twocopy (2回コピー) / uninit1k (MaybeUninit inline)
#
#   shared_mem                - [padded,  twocopy]  (baseline)
#   shared_mem_compact        - [compact, twocopy]
#   shared_mem_uninit1k       - [padded,  uninit1k]
#   shared_mem_compact_uninit1k - [compact, uninit1k]
TRANSPORTS="shared_mem shared_mem_compact shared_mem_uninit1k shared_mem_compact_uninit1k named_pipe unix_socket tcp_socket websocket"

ROUNDS=5
WARMUP=100

run_scenario() {
    local label="$1"
    local size="$2"
    local count="$3"
    local total_msgs=$(( WARMUP + ROUNDS * count ))

    echo "========================================="
    echo " $label (size=${size}B x ${count}, ${ROUNDS} rounds, warmup=${WARMUP})"
    echo "========================================="
    for t in $TRANSPORTS; do
        cargo run --release --bin ipc-server -- -t "$t" --count "$total_msgs" &
        sleep 0.3
        cargo run --release --bin ipc-client -- -t "$t" --count "$count" --size "$size" --rounds "$ROUNDS" --warmup "$WARMUP" 2>&1
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
