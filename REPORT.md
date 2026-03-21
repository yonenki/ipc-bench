# IPC Transport Benchmark Report

## 概要

Rust で複数の IPC トランスポートを実装し、PingPong (送信→エコー→受信) パターンで
レイテンシとスループットを計測した。

### トランスポート一覧

| Transport | 方式 | syscall/msg | 特徴 |
|-----------|------|-------------|------|
| **SharedMem** | mmap + SPSC lock-free ring buffer | **0** | AtomicU64 カーソル + spin-wait, cache line padded |
| **SharedMem(uninit1k)** | 同上 + MaybeUninit inline frame | **0** | 1KB以下のメッセージで初期化スキップ |
| **SharedMem(inline1k)** | 同上 + ゼロ初期化 inline frame | **0** | uninit との比較用 |
| **SharedMem(compact)** | 同上、パディングなし | **0** | false sharing 比較用 |
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
| ベンチ方式 | 5ラウンド中央値、100回ウォームアップ |

---

## 結果

### Scenario 1: 高頻度小メッセージ (64B x 100,000)

小さなメッセージを大量にやり取りするケース。制御コマンド送受信などを想定。

| Transport | p50 | p95 | p99 | max | Throughput |
|-----------|-----|-----|-----|-----|------------|
| **SharedMem(uninit1k)** | **581ns** | **881ns** | **4.57µs** | 411µs | **170 MB/s** |
| SharedMem(compact) | 712ns | 1.62µs | 4.13µs | 34µs | 136 MB/s |
| SharedMem(inline1k) | 882ns | 1.17µs | 5.53µs | 21µs | 117 MB/s |
| SharedMem (baseline) | 1.20µs | 1.57µs | 6.44µs | 64µs | 90 MB/s |
| NamedPipe | 12.85µs | 16.92µs | 21.39µs | 577µs | 9.01 MB/s |
| UnixSocket | 13.43µs | 16.87µs | 22.61µs | 449µs | 8.67 MB/s |
| WebSocket | 19.28µs | 26.44µs | 31.82µs | 438µs | 5.86 MB/s |
| TcpSocket | 20.08µs | 29.57µs | 34.06µs | 463µs | 5.65 MB/s |

**SharedMem(uninit1k) が他トランスポートの 19〜30 倍速い**。

SharedMem バリエーション間の比較:
- **uninit vs inline**: MaybeUninit によるゼロ初期化スキップで 882ns → 581ns (34%改善)
- **padded vs compact**: cache line パディングで p99 が 4.13µs vs 6.44µs
- **baseline**: 2回コピーが最もシンプルだがオーバーヘッドがある

### Scenario 2: 大容量転送 (1MB x 100)

画像やバイナリデータの転送を想定。

| Transport | p50 | p95 | p99 | Throughput |
|-----------|-----|-----|-----|------------|
| UnixSocket | 462µs | 736µs | 1.84ms | **3,986 MB/s** |
| TcpSocket | 600µs | 797µs | 1.11ms | 3,209 MB/s |
| SharedMem(compact) | 802µs | 1.18ms | 1.25ms | 2,372 MB/s |
| SharedMem(inline1k) | 794µs | 1.24ms | 1.40ms | 2,372 MB/s |
| SharedMem(uninit1k) | 812µs | 1.20ms | 1.27ms | 2,321 MB/s |
| SharedMem (baseline) | 819µs | 1.44ms | 1.62ms | 2,206 MB/s |
| NamedPipe | 1.10ms | 1.64ms | 2.22ms | 1,704 MB/s |
| WebSocket | 2.18ms | 3.00ms | 3.55ms | 905 MB/s |

大容量ではカーネルの最適化された memcpy を使う UnixSocket/TcpSocket がスループットで上回る。
inline/uninit の差は 1MB では閾値 (1KB) を超えるため出ない。

### Scenario 3: 超大容量転送 (10MB x 20)

大規模データの一括転送を想定。

| Transport | p50 | p95 | Throughput |
|-----------|-----|-----|------------|
| **SharedMem(uninit1k)** | **3.95ms** | 5.24ms | **5,074 MB/s** |
| SharedMem(inline1k) | 3.96ms | 4.97ms | 5,023 MB/s |
| SharedMem(compact) | 3.97ms | 5.06ms | 5,002 MB/s |
| SharedMem (baseline) | 4.08ms | 4.76ms | 4,971 MB/s |
| TcpSocket | 5.68ms | 7.51ms | 3,401 MB/s |
| UnixSocket | 5.85ms | 6.65ms | 3,337 MB/s |
| NamedPipe | 14.86ms | 15.52ms | 1,361 MB/s |
| WebSocket | 27.52ms | 32.50ms | 718 MB/s |

ウォームアップにより SharedMem が **5 GB/s** に到達。10MB は 16MB リングに収まるため
syscall ゼロ + memcpy の効率が最大限活きる。

---

## 総合分析

### レイテンシ階層構造

```
メモリ読み書き (uninit)    ~581ns    SharedMem(uninit1k)
メモリ読み書き (padded)    ~712ns    SharedMem(compact)
メモリ読み書き (baseline)  ~1.2µs    SharedMem
  + syscall (read/write)    ~13µs    NamedPipe / UnixSocket
  + TCP/IP スタック          ~20µs    TcpSocket / WebSocket
```

### ワークロード別の最適解

| ワークロード | 最適な Transport | 理由 |
|-------------|-----------------|------|
| 高頻度・小メッセージ | **SharedMem(uninit1k)** | syscall ゼロ + MaybeUninit で sub-µs |
| 大容量バルク転送 | **UnixSocket** | カーネルの最適化された memcpy |
| 超大容量 (リングに収まるサイズ) | **SharedMem** | 5 GB/s、syscall ゼロの真価 |
| バランス型 | **SharedMem** | 全シナリオで上位 |
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
   - RingLayout trait でレイアウトをジェネリクス化し型パラメータで切り替え可能

6. **MaybeUninit によるゼロ初期化スキップ**
   - inline frame で length + payload をまとめる際、`[0u8; N]` のゼロクリアがボトルネック
   - `MaybeUninit::uninit()` + `copy_nonoverlapping` で初期化をスキップ
   - 64B メッセージで 882ns → 581ns (34%改善)
   - 閾値が大きすぎる (8KB) とゼロ初期化コストが支配的になり逆効果

7. **ウォームアップと複数ラウンドの重要性**
   - キャッシュが冷えた状態の初回は 2 倍近く遅い
   - 1回計測では CPU クロック変動・スケジューラノイズで結果がブレる
   - 5ラウンド中央値 + 100回ウォームアップで安定した計測が可能に

---

## 再現方法

```bash
# ビルド
cargo build --release

# 全シナリオ実行 (5ラウンド中央値 + ウォームアップ)
./bench.sh all

# 個別シナリオ
./bench.sh quick     # 高頻度小メッセージのみ
./bench.sh large     # 大容量 (1MB) のみ
./bench.sh vlarge    # 超大容量 (10MB) のみ

# 手動実行 (カスタムパラメータ)
cargo run --release --bin ipc-server -- -t shared_mem --count 5100 &
cargo run --release --bin ipc-client -- -t shared_mem --count 1000 --size 64 --rounds 5 --warmup 100

# SharedMem バリエーション
#   shared_mem          - padded, 2回コピー (baseline)
#   shared_mem_compact  - compact (false sharing あり)
#   shared_mem_uninit1k - padded + MaybeUninit inline (1KB閾値, 最速)
#   shared_mem_inline1k - padded + ゼロ初期化 inline (1KB閾値, 比較用)
```
