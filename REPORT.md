# IPC Transport Benchmark Report

## 概要

Rust で 6 種類の IPC トランスポートを実装し、PingPong (送信→エコー→受信) パターンで
レイテンシとスループットを計測した。

### トランスポート一覧

| Transport | 方式 | syscall/msg | 特徴 |
|-----------|------|-------------|------|
| **SharedMem** | mmap + SPSC lock-free ring buffer | **0** | AtomicU64 カーソル + spin-wait, cache line padded |
| **SharedMem(compact)** | 同上 | **0** | パディングなし版 (false sharing 比較用) |
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
| **SharedMem** | **571ns** | **912ns** | **4.88µs** | 174µs | **154 MB/s** |
| SharedMem(compact) | 712ns | 841ns | 8.31µs | 61µs | 135 MB/s |
| NamedPipe | 12.22µs | 15.40µs | 18.19µs | 475µs | 9.64 MB/s |
| UnixSocket | 13.40µs | 16.26µs | 21.03µs | 189µs | 8.84 MB/s |
| TcpSocket | 19.20µs | 23.33µs | 29.46µs | 300µs | 6.14 MB/s |
| WebSocket | 19.15µs | 25.56µs | 30.25µs | 269µs | 6.05 MB/s |

**SharedMem (padded) が他の 16〜27 倍速い**。syscall が一切発生せず、メモリ読み書きのみで完結するため。
cache line パディングにより compact 版と比較して p50 で 20% 改善、p99 で 41% 改善。

### Scenario 2: 大容量転送 (1MB x 100)

画像やバイナリデータの転送を想定。

| Transport | p50 | p95 | p99 | Throughput |
|-----------|-----|-----|-----|------------|
| TcpSocket | 419µs | 562µs | 2.19ms | 4,423 MB/s |
| UnixSocket | 540µs | 852µs | 1.94ms | 3,412 MB/s |
| **SharedMem** | **409µs** | 1.94ms | 2.49ms | **3,012 MB/s** |
| SharedMem(compact) | 442µs | 2.00ms | 2.96ms | 2,822 MB/s |
| NamedPipe | 1.11ms | 1.95ms | 3.22ms | 1,602 MB/s |
| WebSocket | 2.27ms | 3.40ms | 7.35ms | 849 MB/s |

大容量ではカーネルの最適化された memcpy を使う TCP/UnixSocket のスループットが高い。
SharedMem は p50 では最速だが、リングバッファの空き待ち (spin-wait) が発生する場面で p95 以降が悪化。

### Scenario 3: 超大容量転送 (10MB x 20)

大規模データの一括転送を想定。

| Transport | p50 | p95 | Throughput |
|-----------|-----|-----|------------|
| **SharedMem** | **3.46ms** | 27.97ms | **3,798 MB/s** |
| SharedMem(compact) | 3.75ms | 29.37ms | 3,505 MB/s |
| UnixSocket | 5.89ms | 14.28ms | 3,053 MB/s |
| TcpSocket | 6.72ms | 14.89ms | 2,672 MB/s |
| NamedPipe | 19.08ms | 26.78ms | 1,031 MB/s |
| WebSocket | 31.87ms | 97.94ms | 557 MB/s |

SharedMem がスループットでトップ。10MB は 16MB リングに収まるため、
一括 memcpy のコピー効率が活きる。

---

## 総合分析

### レイテンシ階層構造

各層が追加するオーバーヘッドが可視化できた:

```
メモリ読み書きのみ        ~571ns    SharedMem (padded)
  + false sharing        ~712ns    SharedMem (compact)
  + syscall (read/write)  ~12µs    NamedPipe / UnixSocket
  + TCP/IP スタック        ~19µs    TcpSocket / WebSocket
```

### ワークロード別の最適解

| ワークロード | 最適な Transport | 理由 |
|-------------|-----------------|------|
| 高頻度・小メッセージ | **SharedMem (padded)** | syscall ゼロ、sub-µs レイテンシ |
| 大容量バルク転送 | **TCP / UnixSocket** | カーネルの最適化された memcpy |
| 超大容量 (リングに収まるサイズ) | **SharedMem** | memcpy + syscall ゼロの組み合わせ |
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

5. **Cache line パディングによる false sharing 防止**
   - write_cursor と read_cursor を別キャッシュライン (64byte) に配置
   - `#[repr(C, align(64))]` でコンパイラがパディングを保証
   - p99 レイテンシが 8.31µs → 4.88µs (41% 改善)、スループット 135 → 154 MB/s (14% 改善)
   - RingLayout trait でレイアウトをジェネリクス化し、padded/compact を型パラメータで切り替え可能

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
# shared_mem: cache line padded (デフォルト)
# shared_mem_compact: パディングなし (比較用)
cargo run --release --bin ipc-server -- -t shared_mem --count 1000 &
cargo run --release --bin ipc-client -- -t shared_mem --count 1000 --size 64
```
