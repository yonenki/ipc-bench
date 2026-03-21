use std::fs::OpenOptions;
use std::hint;
use std::io;
use std::marker::PhantomData;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::Duration;

use memmap2::MmapMut;

use super::{Role, Transport};

// ---------------------------------------------------------------------------
// リングヘッダのレイアウト定義
// ---------------------------------------------------------------------------

/// リングヘッダの共通インターフェース
///
/// write_cursor と read_cursor の配置方法を抽象化する。
/// 実装ごとにメモリ上のオフセットが異なる。
pub trait RingLayout: Send + 'static {
    /// ヘッダ全体のサイズ (この後にリングデータが続く)
    const HEADER_SIZE: usize;

    /// write_cursor のヘッダ先頭からのオフセット
    const WRITE_CURSOR_OFFSET: usize;

    /// read_cursor のヘッダ先頭からのオフセット
    const READ_CURSOR_OFFSET: usize;

    /// レポート用の表示名サフィックス
    const LABEL: &'static str;
}

/// Padded レイアウト: cache line (64byte) 分離
///
/// ```text
/// offset 0:   write_cursor (AtomicU64)
/// offset 8-63:  padding
/// offset 64:  read_cursor (AtomicU64)
/// offset 72-127: padding
/// offset 128: ring data...
/// ```
///
/// Writer と Reader が別コアで動く場合、同じキャッシュラインに
/// 両カーソルがあると、片方が書くたびにもう片方のキャッシュが
/// 無効化される (false sharing)。64byte 間隔に離すことで防止。
#[repr(C, align(64))]
pub struct CacheLineCursor {
    pub value: AtomicU64,
}

#[repr(C)]
pub struct PaddedHeader {
    pub write_cursor: CacheLineCursor, // offset 0, size 64 (align 64)
    pub read_cursor: CacheLineCursor,  // offset 64, size 64
}

pub struct PaddedLayout;

impl RingLayout for PaddedLayout {
    const HEADER_SIZE: usize = size_of::<PaddedHeader>(); // 128
    const WRITE_CURSOR_OFFSET: usize = 0;
    const READ_CURSOR_OFFSET: usize = 64;
    const LABEL: &'static str = "SharedMem";
}

/// Compact レイアウト: パディングなし
///
/// ```text
/// offset 0:  write_cursor (AtomicU64)
/// offset 8:  read_cursor (AtomicU64)
/// offset 16: ring data...
/// ```
///
/// 同一キャッシュラインに両カーソルが乗るため false sharing が発生するが、
/// ヘッダが小さい分メモリ効率が良い。比較用。
#[repr(C)]
pub struct CompactHeader {
    pub write_cursor: AtomicU64, // offset 0
    pub read_cursor: AtomicU64,  // offset 8
}

pub struct CompactLayout;

impl RingLayout for CompactLayout {
    const HEADER_SIZE: usize = size_of::<CompactHeader>(); // 16
    const WRITE_CURSOR_OFFSET: usize = 0;
    const READ_CURSOR_OFFSET: usize = 8;
    const LABEL: &'static str = "SharedMem(compact)";
}

// ---------------------------------------------------------------------------
// トランスポート本体
// ---------------------------------------------------------------------------

// 64MB に拡大したところ TLB ミス/キャッシュ効率低下で逆に悪化した。
// 16MB がキャッシュ局所性と spin-wait 頻度のバランスが良い。
const RING_DATA_SIZE: usize = 16 * 1024 * 1024; // 16MB
const MSG_HEADER_SIZE: usize = 4;

/// 共有メモリ上の SPSC リングバッファによるトランスポート
///
/// L: レイアウト (PaddedLayout or CompactLayout) で cache line パディングを切り替え
///
/// 双方向通信のため、1ファイル内にリングを2本並べる:
///   [Ring A: Server→Client] [Ring B: Client→Server]
///
/// カーソルは累積バイト数。実際の位置は cursor % RING_DATA_SIZE で求める。
/// メッセージは 4byte LE length + payload のフレーミング。
pub struct SharedMemTransport<L: RingLayout> {
    mmap: MmapMut,
    /// 自分が書くリングの先頭オフセット
    write_ring_offset: usize,
    /// 自分が読むリングの先頭オフセット
    read_ring_offset: usize,
    _layout: PhantomData<L>,
}

/// デフォルト: padded レイアウト
pub type SharedMemPadded = SharedMemTransport<PaddedLayout>;
/// 比較用: compact レイアウト (false sharing あり)
pub type SharedMemCompact = SharedMemTransport<CompactLayout>;

impl<L: RingLayout> SharedMemTransport<L> {
    /// 1方向分のリングの合計サイズ
    const RING_TOTAL_SIZE: usize = L::HEADER_SIZE + RING_DATA_SIZE;
    /// 共有メモリ全体のサイズ (2リング分)
    const SHM_SIZE: usize = Self::RING_TOTAL_SIZE * 2;

    fn shm_path(name: &str) -> PathBuf {
        PathBuf::from(format!("/dev/shm/ipc_bench_{}", name))
    }

    unsafe fn atomic_at(mmap: &MmapMut, offset: usize) -> &AtomicU64 {
        unsafe {
            let ptr = mmap.as_ptr().add(offset) as *const AtomicU64;
            &*ptr
        }
    }

    fn write_cursor(&self) -> &AtomicU64 {
        unsafe { Self::atomic_at(&self.mmap, self.write_ring_offset + L::WRITE_CURSOR_OFFSET) }
    }

    fn write_read_cursor(&self) -> &AtomicU64 {
        unsafe { Self::atomic_at(&self.mmap, self.write_ring_offset + L::READ_CURSOR_OFFSET) }
    }

    fn read_write_cursor(&self) -> &AtomicU64 {
        unsafe { Self::atomic_at(&self.mmap, self.read_ring_offset + L::WRITE_CURSOR_OFFSET) }
    }

    fn read_cursor(&self) -> &AtomicU64 {
        unsafe { Self::atomic_at(&self.mmap, self.read_ring_offset + L::READ_CURSOR_OFFSET) }
    }

    /// リングバッファにデータを書く (wrap-around 対応)
    ///
    /// --- v1: 1バイトずつ write_volatile ---
    /// シンプルだが、大容量データで致命的に遅い。
    /// 1MB のデータで 100万回の write_volatile が発生し、
    /// カーネルの write syscall 内で memcpy する方式（TCP/UnixSocket）に大敗した。
    ///
    /// fn ring_write_bytes(&self, ring_offset: usize, cursor: u64, data: &[u8]) {
    ///     let base = self.mmap.as_ptr() as *mut u8;
    ///     for (i, &b) in data.iter().enumerate() {
    ///         let pos = ring_offset + HEADER_SIZE + ((cursor as usize + i) % RING_DATA_SIZE);
    ///         unsafe { base.add(pos).write_volatile(b) };
    ///     }
    /// }
    ///
    /// --- v2: copy_nonoverlapping でまとめてコピー ---
    /// wrap 境界をまたぐ場合は2回に分けるが、それぞれは memcpy 相当の一括コピー。
    fn ring_write_bytes(&self, ring_offset: usize, cursor: u64, data: &[u8]) {
        let base = self.mmap.as_ptr() as *mut u8;
        let start = (cursor as usize) % RING_DATA_SIZE;
        let ring_start = ring_offset + L::HEADER_SIZE;

        let first_len = data.len().min(RING_DATA_SIZE - start);
        unsafe {
            std::ptr::copy_nonoverlapping(data.as_ptr(), base.add(ring_start + start), first_len);
        }
        // wrap-around: 残りがあればリング先頭にコピー
        if first_len < data.len() {
            let rest = data.len() - first_len;
            unsafe {
                std::ptr::copy_nonoverlapping(
                    data.as_ptr().add(first_len),
                    base.add(ring_start),
                    rest,
                );
            }
        }
    }

    /// リングバッファからデータを読む (wrap-around 対応)
    ///
    /// --- v1: 1バイトずつ read_volatile ---
    /// (v1 のコードは ring_write_bytes のコメント参照。同じ問題。)
    ///
    /// --- v2: copy_nonoverlapping でまとめてコピー ---
    fn ring_read_bytes(&self, ring_offset: usize, cursor: u64, buf: &mut [u8]) {
        let base = self.mmap.as_ptr();
        let start = (cursor as usize) % RING_DATA_SIZE;
        let ring_start = ring_offset + L::HEADER_SIZE;

        let first_len = buf.len().min(RING_DATA_SIZE - start);
        unsafe {
            std::ptr::copy_nonoverlapping(
                base.add(ring_start + start),
                buf.as_mut_ptr(),
                first_len,
            );
        }
        if first_len < buf.len() {
            let rest = buf.len() - first_len;
            unsafe {
                std::ptr::copy_nonoverlapping(
                    base.add(ring_start),
                    buf.as_mut_ptr().add(first_len),
                    rest,
                );
            }
        }
    }
}

impl<L: RingLayout> Transport for SharedMemTransport<L> {
    fn open(name: &str, role: Role) -> io::Result<Self> {
        let path = Self::shm_path(name);

        match role {
            Role::Server => {
                let file = OpenOptions::new()
                    .read(true)
                    .write(true)
                    .create(true)
                    .truncate(true)
                    .open(&path)?;
                file.set_len(Self::SHM_SIZE as u64)?;

                let mmap = unsafe { MmapMut::map_mut(&file)? };

                // 全カーソルを 0 に初期化
                let wc_a = unsafe { Self::atomic_at(&mmap, L::WRITE_CURSOR_OFFSET) };
                let rc_a = unsafe { Self::atomic_at(&mmap, L::READ_CURSOR_OFFSET) };
                let wc_b =
                    unsafe { Self::atomic_at(&mmap, Self::RING_TOTAL_SIZE + L::WRITE_CURSOR_OFFSET) };
                let rc_b =
                    unsafe { Self::atomic_at(&mmap, Self::RING_TOTAL_SIZE + L::READ_CURSOR_OFFSET) };
                wc_a.store(0, Ordering::Release);
                rc_a.store(0, Ordering::Release);
                wc_b.store(0, Ordering::Release);
                rc_b.store(0, Ordering::Release);

                Ok(Self {
                    mmap,
                    write_ring_offset: 0,
                    read_ring_offset: Self::RING_TOTAL_SIZE,
                    _layout: PhantomData,
                })
            }
            Role::Client => {
                let mut retries = 0;
                loop {
                    if Path::new(&path).exists() {
                        if std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0)
                            >= Self::SHM_SIZE as u64
                        {
                            break;
                        }
                    }
                    if retries >= 100 {
                        return Err(io::Error::new(
                            io::ErrorKind::TimedOut,
                            "Shared memory not created by server",
                        ));
                    }
                    retries += 1;
                    thread::sleep(Duration::from_millis(10));
                }

                let file = OpenOptions::new().read(true).write(true).open(&path)?;
                let mmap = unsafe { MmapMut::map_mut(&file)? };

                Ok(Self {
                    mmap,
                    write_ring_offset: Self::RING_TOTAL_SIZE,
                    read_ring_offset: 0,
                    _layout: PhantomData,
                })
            }
        }
    }

    fn send(&mut self, buf: &[u8]) -> io::Result<()> {
        let msg_len = MSG_HEADER_SIZE + buf.len();

        if msg_len > RING_DATA_SIZE {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "Message too large: {} bytes (max {})",
                    buf.len(),
                    RING_DATA_SIZE - MSG_HEADER_SIZE
                ),
            ));
        }

        // 空きができるまで spin-wait
        let wc = loop {
            let wc = self.write_cursor().load(Ordering::Relaxed);
            let rc = self.write_read_cursor().load(Ordering::Acquire);
            let used = wc - rc;
            if (RING_DATA_SIZE as u64 - used) >= msg_len as u64 {
                break wc;
            }
            hint::spin_loop();
        };

        // length header + payload を書く
        let len_bytes = (buf.len() as u32).to_le_bytes();
        self.ring_write_bytes(self.write_ring_offset, wc, &len_bytes);
        self.ring_write_bytes(self.write_ring_offset, wc + MSG_HEADER_SIZE as u64, buf);

        // カーソルを進める (Release)
        self.write_cursor()
            .store(wc + msg_len as u64, Ordering::Release);

        Ok(())
    }

    fn recv(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        // length header が来るまで待つ
        let rc = loop {
            let wc = self.read_write_cursor().load(Ordering::Acquire);
            let rc = self.read_cursor().load(Ordering::Relaxed);
            if wc - rc >= MSG_HEADER_SIZE as u64 {
                break rc;
            }
            hint::spin_loop();
        };

        // length を読む
        let mut len_bytes = [0u8; 4];
        self.ring_read_bytes(self.read_ring_offset, rc, &mut len_bytes);
        let data_len = u32::from_le_bytes(len_bytes) as usize;

        // payload 全体が書かれるまで待つ
        loop {
            let wc = self.read_write_cursor().load(Ordering::Acquire);
            if wc - rc >= (MSG_HEADER_SIZE + data_len) as u64 {
                break;
            }
            hint::spin_loop();
        }

        // payload を読む
        let read_len = data_len.min(buf.len());
        self.ring_read_bytes(
            self.read_ring_offset,
            rc + MSG_HEADER_SIZE as u64,
            &mut buf[..read_len],
        );

        // カーソルを進める
        self.read_cursor()
            .store(rc + (MSG_HEADER_SIZE + data_len) as u64, Ordering::Release);

        Ok(read_len)
    }

    fn cleanup(name: &str) -> io::Result<()> {
        let path = Self::shm_path(name);
        match std::fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e),
        }
    }

    fn transport_name() -> &'static str {
        L::LABEL
    }
}
