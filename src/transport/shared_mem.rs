use std::fs::OpenOptions;
use std::hint;
use std::io;
use std::marker::PhantomData;
use std::mem::MaybeUninit;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::Duration;

use memmap2::MmapMut;

use super::{Role, Transport};

// ---------------------------------------------------------------------------
// 軸1: ヘッダレイアウト — カーソルの物理配置を決める
// ---------------------------------------------------------------------------

/// リングヘッダの物理レイアウトを定義する trait
pub trait HeaderLayout: Send + 'static {
    const HEADER_SIZE: usize;
    const WRITE_CURSOR_OFFSET: usize;
    const READ_CURSOR_OFFSET: usize;
}

/// Padded: write_cursor と read_cursor を別キャッシュラインに配置 (false sharing 防止)
#[repr(C, align(64))]
pub struct CacheLineCursor {
    pub value: AtomicU64,
}

#[repr(C)]
pub struct PaddedHeader {
    pub write_cursor: CacheLineCursor,
    pub read_cursor: CacheLineCursor,
}

pub struct Padded;
impl HeaderLayout for Padded {
    const HEADER_SIZE: usize = size_of::<PaddedHeader>(); // 128
    const WRITE_CURSOR_OFFSET: usize = 0;
    const READ_CURSOR_OFFSET: usize = 64;
}

/// Compact: パディングなし (false sharing あり、比較用)
#[repr(C)]
pub struct CompactHeader {
    pub write_cursor: AtomicU64,
    pub read_cursor: AtomicU64,
}

pub struct Compact;
impl HeaderLayout for Compact {
    const HEADER_SIZE: usize = size_of::<CompactHeader>(); // 16
    const WRITE_CURSOR_OFFSET: usize = 0;
    const READ_CURSOR_OFFSET: usize = 8;
}

// ---------------------------------------------------------------------------
// 軸2: フレーム戦略 — send/recv でのデータ結合方式を決める
// ---------------------------------------------------------------------------

/// length header + payload の書き込み/読み出し戦略
///
/// コンパイル時に const で分岐するため、不要なパスはオプティマイザが除去する (ゼロコスト)。
pub trait FrameStrategy: Send + 'static {
    const LABEL_SUFFIX: &'static str;

    /// フレームを組み立ててリングに書く
    fn write_frame(
        ring: &RingOps,
        ring_offset: usize,
        cursor: u64,
        len_bytes: &[u8; MSG_HEADER_SIZE],
        payload: &[u8],
    );

    /// リングからフレームを読み出して buf に payload を書く
    fn read_frame(
        ring: &RingOps,
        ring_offset: usize,
        cursor: u64,
        msg_len: usize,
        buf: &mut [u8],
        data_len: usize,
    );
}

/// TwoCopy: length と payload を別々に ring_write_bytes (従来方式)
pub struct TwoCopy;
impl FrameStrategy for TwoCopy {
    const LABEL_SUFFIX: &'static str = "";

    #[inline(always)]
    fn write_frame(
        ring: &RingOps,
        ring_offset: usize,
        cursor: u64,
        len_bytes: &[u8; MSG_HEADER_SIZE],
        payload: &[u8],
    ) {
        ring.write_bytes(ring_offset, cursor, len_bytes);
        ring.write_bytes(ring_offset, cursor + MSG_HEADER_SIZE as u64, payload);
    }

    #[inline(always)]
    fn read_frame(
        ring: &RingOps,
        ring_offset: usize,
        cursor: u64,
        _msg_len: usize,
        buf: &mut [u8],
        data_len: usize,
    ) {
        let read_len = data_len.min(buf.len());
        ring.read_bytes(
            ring_offset,
            cursor + MSG_HEADER_SIZE as u64,
            &mut buf[..read_len],
        );
    }
}

/// InlineZeroed<FRAME_SIZE>: length + payload をゼロ初期化スタックバッファにまとめて1回コピー
///
/// FRAME_SIZE は MSG_HEADER_SIZE + payload閾値。const generic の制約で
/// 「MSG_HEADER_SIZE + N」のような式が使えないため、合計値を直接渡す。
pub struct InlineZeroed<const FRAME_SIZE: usize>;
impl<const FRAME_SIZE: usize> FrameStrategy for InlineZeroed<FRAME_SIZE> {
    const LABEL_SUFFIX: &'static str = match FRAME_SIZE - MSG_HEADER_SIZE {
        256 => "(inline256)",
        512 => "(inline512)",
        1024 => "(inline1k)",
        2048 => "(inline2k)",
        4096 => "(inline4k)",
        8192 => "(inline8k)",
        _ => "(inline?)",
    };

    #[inline(always)]
    fn write_frame(
        ring: &RingOps,
        ring_offset: usize,
        cursor: u64,
        len_bytes: &[u8; MSG_HEADER_SIZE],
        payload: &[u8],
    ) {
        if payload.len() + MSG_HEADER_SIZE <= FRAME_SIZE {
            let msg_len = MSG_HEADER_SIZE + payload.len();
            let mut frame = [0u8; FRAME_SIZE];
            frame[..MSG_HEADER_SIZE].copy_from_slice(len_bytes);
            frame[MSG_HEADER_SIZE..msg_len].copy_from_slice(payload);
            ring.write_bytes(ring_offset, cursor, &frame[..msg_len]);
        } else {
            TwoCopy::write_frame(ring, ring_offset, cursor, len_bytes, payload);
        }
    }

    #[inline(always)]
    fn read_frame(
        ring: &RingOps,
        ring_offset: usize,
        cursor: u64,
        msg_len: usize,
        buf: &mut [u8],
        data_len: usize,
    ) {
        if data_len + MSG_HEADER_SIZE <= FRAME_SIZE {
            let mut frame = [0u8; FRAME_SIZE];
            ring.read_bytes(ring_offset, cursor, &mut frame[..msg_len]);
            let read_len = data_len.min(buf.len());
            buf[..read_len].copy_from_slice(&frame[MSG_HEADER_SIZE..MSG_HEADER_SIZE + read_len]);
        } else {
            TwoCopy::read_frame(ring, ring_offset, cursor, msg_len, buf, data_len);
        }
    }
}

/// InlineUninit<FRAME_SIZE>: MaybeUninit でゼロ初期化をスキップする inline frame
pub struct InlineUninit<const FRAME_SIZE: usize>;
impl<const FRAME_SIZE: usize> FrameStrategy for InlineUninit<FRAME_SIZE> {
    const LABEL_SUFFIX: &'static str = match FRAME_SIZE - MSG_HEADER_SIZE {
        256 => "(uninit256)",
        512 => "(uninit512)",
        1024 => "(uninit1k)",
        2048 => "(uninit2k)",
        4096 => "(uninit4k)",
        8192 => "(uninit8k)",
        _ => "(uninit?)",
    };

    #[inline(always)]
    fn write_frame(
        ring: &RingOps,
        ring_offset: usize,
        cursor: u64,
        len_bytes: &[u8; MSG_HEADER_SIZE],
        payload: &[u8],
    ) {
        if payload.len() + MSG_HEADER_SIZE <= FRAME_SIZE {
            let msg_len = MSG_HEADER_SIZE + payload.len();
            let mut frame: [MaybeUninit<u8>; FRAME_SIZE] =
                unsafe { MaybeUninit::uninit().assume_init() };
            unsafe {
                let ptr = frame.as_mut_ptr() as *mut u8;
                std::ptr::copy_nonoverlapping(len_bytes.as_ptr(), ptr, MSG_HEADER_SIZE);
                std::ptr::copy_nonoverlapping(payload.as_ptr(), ptr.add(MSG_HEADER_SIZE), payload.len());
            }
            let slice = unsafe { std::slice::from_raw_parts(frame.as_ptr() as *const u8, msg_len) };
            ring.write_bytes(ring_offset, cursor, slice);
        } else {
            TwoCopy::write_frame(ring, ring_offset, cursor, len_bytes, payload);
        }
    }

    #[inline(always)]
    fn read_frame(
        ring: &RingOps,
        ring_offset: usize,
        cursor: u64,
        msg_len: usize,
        buf: &mut [u8],
        data_len: usize,
    ) {
        if data_len + MSG_HEADER_SIZE <= FRAME_SIZE {
            let mut frame: [MaybeUninit<u8>; FRAME_SIZE] =
                unsafe { MaybeUninit::uninit().assume_init() };
            let slice = unsafe {
                std::slice::from_raw_parts_mut(frame.as_mut_ptr() as *mut u8, msg_len)
            };
            ring.read_bytes(ring_offset, cursor, slice);
            let read_len = data_len.min(buf.len());
            buf[..read_len].copy_from_slice(&slice[MSG_HEADER_SIZE..MSG_HEADER_SIZE + read_len]);
        } else {
            TwoCopy::read_frame(ring, ring_offset, cursor, msg_len, buf, data_len);
        }
    }
}

// ---------------------------------------------------------------------------
// リングバッファ操作 — mmap 上の read/write を提供
// ---------------------------------------------------------------------------

const RING_DATA_SIZE: usize = 16 * 1024 * 1024; // 16MB
const MSG_HEADER_SIZE: usize = 4;

/// mmap 上のリングバッファ読み書き操作
///
/// FrameStrategy から呼ばれる。トランスポート本体から分離することで
/// FrameStrategy が &self を取らずに済む（ジェネリクスの制約回避）。
pub struct RingOps {
    ptr: *const u8,
    header_size: usize,
}

impl RingOps {
    fn new(mmap: &MmapMut, header_size: usize) -> Self {
        Self {
            ptr: mmap.as_ptr(),
            header_size,
        }
    }

    /// wrap-around 対応の書き込み (copy_nonoverlapping)
    #[inline(always)]
    pub fn write_bytes(&self, ring_offset: usize, cursor: u64, data: &[u8]) {
        let base = self.ptr as *mut u8;
        let start = (cursor as usize) % RING_DATA_SIZE;
        let ring_start = ring_offset + self.header_size;

        let first_len = data.len().min(RING_DATA_SIZE - start);
        unsafe {
            std::ptr::copy_nonoverlapping(data.as_ptr(), base.add(ring_start + start), first_len);
        }
        if first_len < data.len() {
            unsafe {
                std::ptr::copy_nonoverlapping(
                    data.as_ptr().add(first_len),
                    base.add(ring_start),
                    data.len() - first_len,
                );
            }
        }
    }

    /// wrap-around 対応の読み出し (copy_nonoverlapping)
    #[inline(always)]
    pub fn read_bytes(&self, ring_offset: usize, cursor: u64, buf: &mut [u8]) {
        let base = self.ptr;
        let start = (cursor as usize) % RING_DATA_SIZE;
        let ring_start = ring_offset + self.header_size;

        let first_len = buf.len().min(RING_DATA_SIZE - start);
        unsafe {
            std::ptr::copy_nonoverlapping(
                base.add(ring_start + start),
                buf.as_mut_ptr(),
                first_len,
            );
        }
        if first_len < buf.len() {
            unsafe {
                std::ptr::copy_nonoverlapping(
                    base.add(ring_start),
                    buf.as_mut_ptr().add(first_len),
                    buf.len() - first_len,
                );
            }
        }
    }
}

// ---------------------------------------------------------------------------
// トランスポート本体
// ---------------------------------------------------------------------------

/// 共有メモリ上の SPSC リングバッファによるトランスポート
///
/// H: ヘッダレイアウト (Padded / Compact)
/// F: フレーム戦略 (TwoCopy / InlineZeroed<N> / InlineUninit<N>)
///
/// 双方向通信のため、1ファイル内にリングを2本並べる:
///   [Ring A: Server→Client] [Ring B: Client→Server]
pub struct SharedMemTransport<H: HeaderLayout, F: FrameStrategy> {
    mmap: MmapMut,
    write_ring_offset: usize,
    read_ring_offset: usize,
    _phantom: PhantomData<(H, F)>,
}

// --- type aliases ---

// ---------------------------------------------------------------------------
// type aliases — HeaderLayout × FrameStrategy の直積
// ---------------------------------------------------------------------------
// FRAME_SIZE = MSG_HEADER_SIZE(4) + payload閾値

// --- 検証用4パターン (2軸 × 2値) ---
// HeaderLayout 軸の効果: Padded vs Compact (同じ FrameStrategy で比較)
// FrameStrategy 軸の効果: TwoCopy vs InlineUninit (同じ HeaderLayout で比較)
pub type SharedMemPadded = SharedMemTransport<Padded, TwoCopy>;
pub type SharedMemCompact = SharedMemTransport<Compact, TwoCopy>;
pub type SharedMemUninit1k = SharedMemTransport<Padded, InlineUninit<1028>>;
pub type SharedMemCompactUninit1k = SharedMemTransport<Compact, InlineUninit<1028>>;

// --- 閾値スイープ用 (Padded 固定) ---
// inline frame (ゼロ初期化)
pub type SharedMemInline = SharedMemTransport<Padded, InlineZeroed<260>>;
pub type SharedMemInline512 = SharedMemTransport<Padded, InlineZeroed<516>>;
pub type SharedMemInline1k = SharedMemTransport<Padded, InlineZeroed<1028>>;
pub type SharedMemInline2k = SharedMemTransport<Padded, InlineZeroed<2052>>;
pub type SharedMemInline4k = SharedMemTransport<Padded, InlineZeroed<4100>>;
pub type SharedMemInline8k = SharedMemTransport<Padded, InlineZeroed<8196>>;

// inline frame (MaybeUninit)
pub type SharedMemUninit = SharedMemTransport<Padded, InlineUninit<260>>;
pub type SharedMemUninit512 = SharedMemTransport<Padded, InlineUninit<516>>;
pub type SharedMemUninit2k = SharedMemTransport<Padded, InlineUninit<2052>>;
pub type SharedMemUninit4k = SharedMemTransport<Padded, InlineUninit<4100>>;
pub type SharedMemUninit8k = SharedMemTransport<Padded, InlineUninit<8196>>;

impl<H: HeaderLayout, F: FrameStrategy> SharedMemTransport<H, F> {
    const RING_TOTAL_SIZE: usize = H::HEADER_SIZE + RING_DATA_SIZE;
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
        unsafe { Self::atomic_at(&self.mmap, self.write_ring_offset + H::WRITE_CURSOR_OFFSET) }
    }

    fn write_read_cursor(&self) -> &AtomicU64 {
        unsafe { Self::atomic_at(&self.mmap, self.write_ring_offset + H::READ_CURSOR_OFFSET) }
    }

    fn read_write_cursor(&self) -> &AtomicU64 {
        unsafe { Self::atomic_at(&self.mmap, self.read_ring_offset + H::WRITE_CURSOR_OFFSET) }
    }

    fn read_cursor(&self) -> &AtomicU64 {
        unsafe { Self::atomic_at(&self.mmap, self.read_ring_offset + H::READ_CURSOR_OFFSET) }
    }

    fn ring_ops(&self) -> RingOps {
        RingOps::new(&self.mmap, H::HEADER_SIZE)
    }

    fn transport_label() -> String {
        let header = if H::HEADER_SIZE == size_of::<PaddedHeader>() {
            "padded"
        } else {
            "compact"
        };
        let strategy = F::LABEL_SUFFIX;
        if strategy.is_empty() {
            format!("SharedMem[{}]", header)
        } else {
            format!("SharedMem[{},{}]", header, strategy)
        }
    }
}

impl<H: HeaderLayout, F: FrameStrategy> Transport for SharedMemTransport<H, F> {
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

                for offset in [0, Self::RING_TOTAL_SIZE] {
                    let wc = unsafe { Self::atomic_at(&mmap, offset + H::WRITE_CURSOR_OFFSET) };
                    let rc = unsafe { Self::atomic_at(&mmap, offset + H::READ_CURSOR_OFFSET) };
                    wc.store(0, Ordering::Release);
                    rc.store(0, Ordering::Release);
                }

                Ok(Self {
                    mmap,
                    write_ring_offset: 0,
                    read_ring_offset: Self::RING_TOTAL_SIZE,
                    _phantom: PhantomData,
                })
            }
            Role::Client => {
                let mut retries = 0;
                loop {
                    if Path::new(&path).exists()
                        && std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0)
                            >= Self::SHM_SIZE as u64
                    {
                        break;
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
                    _phantom: PhantomData,
                })
            }
        }
    }

    fn send(&mut self, buf: &[u8]) -> io::Result<()> {
        let msg_len = MSG_HEADER_SIZE + buf.len();

        if msg_len > RING_DATA_SIZE {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("Message too large: {} bytes (max {})", buf.len(), RING_DATA_SIZE - MSG_HEADER_SIZE),
            ));
        }

        // 空きができるまで spin-wait
        let wc = loop {
            let wc = self.write_cursor().load(Ordering::Relaxed);
            let rc = self.write_read_cursor().load(Ordering::Acquire);
            if (RING_DATA_SIZE as u64 - (wc - rc)) >= msg_len as u64 {
                break wc;
            }
            hint::spin_loop();
        };

        // FrameStrategy に委譲 — コンパイル時にインライン展開される
        let len_bytes = (buf.len() as u32).to_le_bytes();
        F::write_frame(&self.ring_ops(), self.write_ring_offset, wc, &len_bytes, buf);

        self.write_cursor().store(wc + msg_len as u64, Ordering::Release);
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
        self.ring_ops().read_bytes(self.read_ring_offset, rc, &mut len_bytes);
        let data_len = u32::from_le_bytes(len_bytes) as usize;
        let msg_len = MSG_HEADER_SIZE + data_len;

        // payload 全体が書かれるまで待つ
        loop {
            let wc = self.read_write_cursor().load(Ordering::Acquire);
            if wc - rc >= msg_len as u64 {
                break;
            }
            hint::spin_loop();
        }

        // FrameStrategy に委譲
        F::read_frame(&self.ring_ops(), self.read_ring_offset, rc, msg_len, buf, data_len);

        self.read_cursor().store(rc + msg_len as u64, Ordering::Release);
        Ok(data_len.min(buf.len()))
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
        // const でないため leak で 'static にする (プロセス寿命で問題なし)
        let label = Self::transport_label();
        Box::leak(label.into_boxed_str())
    }
}
