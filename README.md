# IPC Transport Benchmark Report

## 概要

Rust で複数の IPC トランスポートを実装し、PingPong (送信→エコー→受信) パターンで
レイテンシとスループットを計測した。

SharedMem は2つの独立した最適化軸をジェネリクスで直積化し、
各軸の効果を独立に検証できる設計になっている。

## トランスポート一覧

### Socket / Pipe 系

| Transport | 方式 | syscall/msg |
|-----------|------|-------------|
| NamedPipe | FIFO 2本 (双方向) | 2 (read+write) |
| UnixSocket | Unix domain socket (SOCK_STREAM) | 2 (read+write) |
| TcpSocket | TCP localhost, set_nodelay + BufWriter | 2 (read+write) |
| WebSocket | tungstenite over TCP | 2 (read+write) |

### SharedMem 系 — `SharedMemTransport<H, F>` の直積

| 表記 | HeaderLayout (H) | FrameStrategy (F) | 特徴 |
|------|------|------|------|
| `SharedMem[padded]` | Padded | TwoCopy | **baseline**: cache line 分離 + 2回コピー |
| `SharedMem[compact]` | Compact | TwoCopy | false sharing あり (H軸の効果を見る) |
| `SharedMem[padded,(uninit1k)]` | Padded | InlineUninit<1028> | MaybeUninit inline frame (F軸の効果を見る) |
| `SharedMem[compact,(uninit1k)]` | Compact | InlineUninit<1028> | 両軸の組み合わせ |

**検証軸:**
- **HeaderLayout 軸**: `Padded` (cache line 64byte 分離) vs `Compact` (8byte 隣接)
  → false sharing の影響を測定
- **FrameStrategy 軸**: `TwoCopy` (length と payload を2回に分けてコピー) vs `InlineUninit` (MaybeUninit スタックバッファに結合して1回コピー)
  → フレーム結合 + ゼロ初期化スキップの効果を測定

全バリエーションは `SharedMemTransport<H, F>` の型パラメータで表現され、
`#[inline(always)]` + const 分岐によりコンパイル時に不要パスが除去される（ゼロコスト抽象化）。

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

| Transport | p50 | p95 | p99 | Throughput |
|-----------|-----|-----|-----|------------|
| **SharedMem[padded]** | **190ns** | **220ns** | **4.06µs** | **418 MB/s** |
| SharedMem[compact,(uninit1k)] | 692ns | 801ns | 3.91µs | 152 MB/s |
| SharedMem[padded,(uninit1k)] | 1.12µs | 1.46µs | 6.11µs | 94 MB/s |
| SharedMem[compact] | 1.65µs | 1.85µs | 6.11µs | 67 MB/s |
| NamedPipe | 12.07µs | 14.84µs | 18.11µs | 9.74 MB/s |
| UnixSocket | 12.94µs | 17.11µs | 26.23µs | 8.97 MB/s |
| TcpSocket | 18.86µs | 22.77µs | 34.13µs | 6.20 MB/s |
| WebSocket | 19.14µs | 28.50µs | 40.62µs | 5.87 MB/s |

**SharedMem[padded] が p50 190ns で最速**。NamedPipe の 63 倍、TCP の 100 倍速い。

SharedMem 軸分析 (64B):

| | TwoCopy | InlineUninit 1k |
|---|---|---|
| **Padded** | **190ns** | 1.12µs |
| **Compact** | 1.65µs | 692ns |

→ 64B ではフレーム結合のオーバーヘッドが TwoCopy のシンプルさに勝てない。Padded+TwoCopy が最速。

### Scenario 2: 大容量転送 (1MB x 100)

| Transport | p50 | p95 | Throughput |
|-----------|-----|-----|------------|
| **SharedMem[padded]** | **398µs** | 721µs | **4,473 MB/s** |
| SharedMem[compact] | 409µs | 666µs | 4,546 MB/s |
| TcpSocket | 435µs | 612µs | 4,484 MB/s |
| SharedMem[padded,(uninit1k)] | 477µs | 893µs | 3,786 MB/s |
| SharedMem[compact,(uninit1k)] | 509µs | 656µs | 3,805 MB/s |
| UnixSocket | 687µs | 1.08ms | 3,099 MB/s |
| NamedPipe | 1.07ms | 1.60ms | 1,763 MB/s |
| WebSocket | 2.15ms | 3.04ms | 898 MB/s |

1MB ではすべての SharedMem バリエーションが高速。
inline 閾値 (1KB) を超えるため InlineUninit は TwoCopy にフォールバックするが、
関数呼び出し分岐のわずかなオーバーヘッドが見える。

### Scenario 3: 超大容量転送 (10MB x 20)

| Transport | p50 | p95 | Throughput |
|-----------|-----|-----|------------|
| **SharedMem[padded,(uninit1k)]** | **3.67ms** | 5.06ms | **5,263 MB/s** |
| SharedMem[padded] | 3.77ms | 5.01ms | 5,213 MB/s |
| SharedMem[compact] | 3.77ms | 4.86ms | 5,169 MB/s |
| SharedMem[compact,(uninit1k)] | 3.79ms | 4.74ms | 5,093 MB/s |
| TcpSocket | 5.57ms | 6.56ms | 3,501 MB/s |
| UnixSocket | 6.18ms | 7.79ms | 3,259 MB/s |
| NamedPipe | 14.49ms | 15.91ms | 1,377 MB/s |
| WebSocket | 27.87ms | 32.19ms | 728 MB/s |

全 SharedMem が **5 GB/s 超**。10MB はリングバッファ (16MB) に収まるため、
syscall ゼロ + memcpy の効率が最大限活きる。

---

## 総合分析

### レイテンシ階層構造

```
メモリ読み書きのみ         ~190ns    SharedMem[padded]
  + false sharing          ~1.6µs    SharedMem[compact]
  + syscall (read/write)    ~12µs    NamedPipe / UnixSocket
  + TCP/IP スタック          ~19µs    TcpSocket / WebSocket
```

### ワークロード別の最適解

| ワークロード | 最適な Transport | 理由 |
|-------------|-----------------|------|
| 高頻度・小メッセージ | **SharedMem[padded]** | syscall ゼロ、sub-µs |
| 大容量バルク転送 | **SharedMem[padded] / TcpSocket** | 4.4~4.5 GB/s で互角 |
| 超大容量 | **SharedMem** (全バリエーション) | 5+ GB/s |
| クロスマシン | **TcpSocket** | ネットワーク越し唯一 |

### 実装で得た知見

1. **syscall は1回あたり ~10µs のコスト**
   - TCP の素朴な実装 (write_all 2回) が WebSocket より遅かった
   - BufWriter で write をまとめて改善

2. **リングバッファサイズは大きすぎてもダメ**
   - 16MB → 64MB で TLB ミス/キャッシュ効率低下により逆に悪化
   - CPU キャッシュとの局所性が重要

3. **1バイトずつ write_volatile vs copy_nonoverlapping**
   - 1MB のデータで 100万回の volatile 書き込みは致命的に遅い
   - memcpy 相当の一括コピーで 3〜4 倍改善

4. **Atomic の Acquire/Release は x86 ではほぼゼロコスト**
   - x86 は TSO (Total Store Order) なのでコンパイラバリアだけで済む

5. **Cache line パディングと CPU コア配置の関係**
   - `#[repr(C, align(64))]` で write_cursor / read_cursor を別キャッシュラインに分離
   - `taskset` でコア配置を固定して false sharing の効果を検証:

   | 配置 | Padded p50 | Compact p50 | 備考 |
   |------|-----------|------------|------|
   | 同一物理コア (SMT) | 90ns | 100ns | L1 共有のため差なし |
   | 別物理コア | 190ns | 221ns | MESI Invalidate で 14% の差 |
   | OS 任せ | 190ns | **701ns** | スケジューラ次第で 7 倍悪化 |

   - SMT ペアは L1 キャッシュを物理的に共有するため、false sharing のペナルティ (MESI Invalidate) が発生しない
   - CI の VM で Padded の効果が出なかったのは、vCPU が同一物理コアの SMT ペアだったため
   - **Padded の本当の価値は OS 任せの環境での安定性** — Compact はスケジューラに依存して不安定

6. **MaybeUninit の効果と限界**
   - inline frame のゼロ初期化 (`[0u8; N]`) がボトルネックになるケースで有効
   - ただし 64B のような極小メッセージでは TwoCopy の方がシンプルで速い
   - フレーム結合のメリットは中サイズ (数百B〜数KB) で出る

7. **ウォームアップと複数ラウンドの重要性**
   - キャッシュが冷えた状態の初回は数倍遅い
   - 5ラウンド中央値 + 100回ウォームアップで安定した計測が可能に

8. **クロスプラットフォーム検証 (GitHub Actions)**
   - Linux / Windows で全トランスポートのベンチマークを CI で自動実行
   - SharedMem: Linux は `/dev/shm/` (tmpfs)、Windows は `%TEMP%` + `memmap2`
   - NamedPipe: Linux は FIFO 2本、Windows は `CreateNamedPipeW` (双方向)
   - Windows の NamedPipe が大容量で 6.5~7 GB/s と Linux FIFO (1 GB/s) の 7 倍速い

9. **ゼロコスト抽象化による検証軸の設計**
   - `HeaderLayout` と `FrameStrategy` を独立した trait で定義
   - `SharedMemTransport<H, F>` の型パラメータで直積化
   - const 分岐 + `#[inline(always)]` でランタイムコストなし
   - `dispatch_transport!` マクロでバリエーション追加が1行

---

## アーキテクチャ

### SharedMem の型構造

```
SharedMemTransport<H: HeaderLayout, F: FrameStrategy>
    │
    ├── HeaderLayout (軸1: カーソル配置)
    │   ├── Padded   — #[repr(C, align(64))] で cache line 分離
    │   └── Compact  — 8byte 隣接 (false sharing)
    │
    ├── FrameStrategy (軸2: フレーム結合方式)
    │   ├── TwoCopy        — length + payload を2回に分けてコピー
    │   ├── InlineZeroed<N> — ゼロ初期化バッファに結合して1回コピー
    │   └── InlineUninit<N> — MaybeUninit で初期化スキップ
    │
    └── RingOps — mmap 上の read/write 操作 (wrap-around 対応)
```

### バリエーション追加手順

1. `shared_mem.rs`: `pub type` を1行追加
2. `lib.rs`: `dispatch_transport!` マクロに1行追加
3. `bench.sh`: `TRANSPORTS` に追加

server / client のコードは変更不要。

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

# 手動実行
cargo run --release --bin ipc-server -- -t shared_mem --count 5100 &
cargo run --release --bin ipc-client -- -t shared_mem --count 1000 --size 64 --rounds 5 --warmup 100
```
