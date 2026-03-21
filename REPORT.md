# IPC Transport Benchmark Report

## 概要

Rust で 5 種類の IPC トランスポートを実装し、PingPong (送信→エコー→受信) パターンで
レイテンシとスループットを計測した。

### トランスポート一覧

| Transport | 方式 | syscall/msg | 特徴 |
|-----------|------|-------------|------|
| **SharedMem** | mmap + SPSC lock-free ring buffer | **0** | AtomicU64 カーソル + spin-wait |
| **NamedPipe** | FIFO 2本 (双方向) | 2 (read+write) | BufWriter で write を1回にまとめ |
| **UnixSocket** | Unix domain socket (SOCK_STREAM) | 2 (read+write) | length-prefix フレーミング |
| **TcpSocket** | TCP localhost (127.0.0.1) | 2 (read+write) | set_nodelay + BufWriter |
| **WebSocket** | tungstenite over TCP | 2 (read+write) | フレーミング/マスキングのオーバーヘッドあり |

## 環境

| 項目 | 値 |
|------|-----|
| CPU | AMD Ryzen 7 5700U (8C/16T, 最大 4.37GHz) |
| メモリ | 16GB DDR4 |
| OS | Ubuntu Linux 6.14.0-37-generic (x86_64) |
| Rust | rustc 1.88.0 (2025-06-23), edition 2024 |
| ビルド | release (optimized) |
| 計測 | `std::time::Instant` (CLOCK_MONOTONIC, ~20ns精度) |

---

## 結果

### Scenario 1: 高頻度小メッセージ (64B x 100,000)

小さなメッセージを大量にやり取りするケース。制御コマンド送受信などを想定。

| Transport | p50 | p95 | p99 | max | Throughput |
|-----------|-----|-----|-----|-----|------------|
| **SharedMem** | **712ns** | **882ns** | **8.52µs** | 119µs | **130 MB/s** |
| NamedPipe | 13.45µs | 16.55µs | 22.77µs | 1.14ms | 8.79 MB/s |
| UnixSocket | 13.38µs | 17.62µs | 24.31µs | 414µs | 8.59 MB/s |
| TcpSocket | 19.72µs | 25.94µs | 37.86µs | 381µs | 5.79 MB/s |
| WebSocket | 19.12µs | 25.62µs | 34.10µs | 330µs | 6.03 MB/s |

**SharedMem が他の 15〜28 倍速い**。syscall が一切発生せず、メモリ読み書きのみで完結するため。

### Scenario 2: 大容量転送 (1MB x 100)

画像やバイナリデータの転送を想定。

| Transport | p50 | p95 | p99 | Throughput |
|-----------|-----|-----|-----|------------|
| TcpSocket | 434µs | 867µs | 1.70ms | 3,984 MB/s |
| UnixSocket | 461µs | 751µs | 1.61ms | 3,947 MB/s |
| **SharedMem** | **428µs** | **2.03ms** | **2.67ms** | **2,874 MB/s** |
| NamedPipe | 1.13ms | 1.93ms | 2.78ms | 1,634 MB/s |
| WebSocket | 2.19ms | 3.46ms | 6.25ms | 850 MB/s |

大容量ではカーネルの最適化された memcpy を使う TCP/UnixSocket が強い。
SharedMem は p50 では互角だが、リングバッファの空き待ち (spin-wait) が発生する場面で p95 以降が悪化。

### Scenario 3: 超大容量転送 (10MB x 20)

大規模データの一括転送を想定。

| Transport | p50 | p95 | Throughput |
|-----------|-----|-----|------------|
| **SharedMem** | 4.36ms | 29.60ms | **3,279 MB/s** |
| UnixSocket | 6.21ms | 12.80ms | 2,970 MB/s |
| TcpSocket | 6.08ms | 14.78ms | 2,808 MB/s |
| NamedPipe | 16.20ms | 25.79ms | 1,218 MB/s |
| WebSocket | 29.03ms | 85.85ms | 619 MB/s |

SharedMem がスループットでトップに返り咲く。10MB は 16MB リングに収まるため、
一括 memcpy のコピー効率が活きる。

---

## 総合分析

### レイテンシ階層構造

各層が追加するオーバーヘッドが可視化できた:

```
メモリ読み書きのみ        ~700ns    SharedMem
  + syscall (read/write)  ~13µs     NamedPipe / UnixSocket
  + TCP/IP スタック        ~20µs     TcpSocket
  + プロトコル層           ~19µs     WebSocket
```

### ワークロード別の最適解

| ワークロード | 最適な Transport | 理由 |
|-------------|-----------------|------|
| 高頻度・小メッセージ | **SharedMem** | syscall ゼロ、sub-µs レイテンシ |
| 大容量バルク転送 | **TCP / UnixSocket** | カーネルの最適化された memcpy |
| バランス型 | **SharedMem** | 小〜中で圧倒的、大容量でも互角以上 |
| クロスマシン必要 | **TcpSocket** | ネットワーク越しに使える唯一の選択肢 |

### 実装で得た知見

1. **syscall は1回あたり ~10µs のコスト**がある
   - TCP の素朴な実装 (write_all 2回) が WebSocket より遅かった原因
   - BufWriter で write をまとめることで改善

2. **SharedMem のリングバッファサイズは大きすぎてもダメ**
   - 16MB → 64MB に拡大したら TLB ミス/キャッシュ効率低下で逆に悪化
   - CPU キャッシュとの局所性が重要

3. **1バイトずつ write_volatile vs copy_nonoverlapping**
   - 1MB のデータで 100万回の volatile 書き込みは致命的に遅い
   - memcpy 相当の一括コピーで 3〜4 倍改善

4. **Atomic の Acquire/Release は x86 ではほぼゼロコスト**
   - x86 は TSO (Total Store Order) なのでコンパイラバリアだけで済む
   - SharedMem の spin-wait ループで Atomic load が高速に動く理由

---

## 再現方法

```bash
# ビルド
cargo build --release

# 全シナリオ実行
./bench.sh all

# 個別シナリオ
./bench.sh quick     # 高頻度小メッセージのみ
./bench.sh large     # 大容量 (1MB) のみ
./bench.sh vlarge    # 超大容量 (10MB) のみ

# 手動実行 (カスタムパラメータ)
cargo run --release --bin ipc-server -- -t shared_mem --count 1000 &
cargo run --release --bin ipc-client -- -t shared_mem --count 1000 --size 64
```
