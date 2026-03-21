//! 共有メモリの実験用バイナリ
//!
//! Step 3: SPSC リングバッファ
//!
//! メモリレイアウト:
//!   offset 0:  write_cursor (AtomicU64) — リングバッファへの累積書き込みバイト数
//!   offset 8:  read_cursor  (AtomicU64) — リングバッファからの累積読み取りバイト数
//!   offset 16: ring buffer data
//!
//! カーソルは累積バイト数で、実際の位置は cursor % RING_SIZE で求める。
//! これにより wrap-around の判定が「write_cursor - read_cursor」の引き算だけで済む。
//!
//! 使い方:
//!   cargo run --bin shm-playground -- server
//!   cargo run --bin shm-playground -- client

use memmap2::MmapMut;
use std::fs::OpenOptions;
use std::hint;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::Duration;

const SHM_PATH: &str = "/dev/shm/ipc_bench_playground";
const HEADER_SIZE: usize = 16; // write_cursor(8) + read_cursor(8)
const RING_SIZE: usize = 1024; // リングバッファのサイズ (小さめにして wrap を観察)
const SHM_SIZE: usize = HEADER_SIZE + RING_SIZE;
const MSG_HEADER_SIZE: usize = 4; // メッセージ長 (u32 LE)

unsafe fn atomic_at(mmap: &MmapMut, offset: usize) -> &AtomicU64 {
    unsafe {
        let ptr = mmap.as_ptr().add(offset) as *const AtomicU64;
        &*ptr
    }
}

/// リングバッファにデータを書き込む (Writer 側)
/// write_cursor を Release で進めることで、Reader にデータの存在を通知する
fn ring_write(mmap: &MmapMut, write_cursor: &AtomicU64, read_cursor: &AtomicU64, data: &[u8]) {
    let msg_len = MSG_HEADER_SIZE + data.len();

    // リングに空きができるまで待つ
    loop {
        let wc = write_cursor.load(Ordering::Relaxed);
        let rc = read_cursor.load(Ordering::Acquire);
        let used = wc - rc;
        if (RING_SIZE as u64 - used) >= msg_len as u64 {
            break;
        }
        hint::spin_loop();
    }

    let wc = write_cursor.load(Ordering::Relaxed);
    let ring_base = mmap.as_ptr() as *mut u8;

    // length header を書く
    let len_bytes = (data.len() as u32).to_le_bytes();
    for (i, &b) in len_bytes.iter().enumerate() {
        let pos = HEADER_SIZE + ((wc as usize + i) % RING_SIZE);
        unsafe { ring_base.add(pos).write_volatile(b) };
    }

    // payload を書く
    for (i, &b) in data.iter().enumerate() {
        let pos = HEADER_SIZE + ((wc as usize + MSG_HEADER_SIZE + i) % RING_SIZE);
        unsafe { ring_base.add(pos).write_volatile(b) };
    }

    // カーソルを進める (Release: 上の書き込みが先に見えることを保証)
    write_cursor.store(wc + msg_len as u64, Ordering::Release);
}

/// リングバッファからデータを読み出す (Reader 側)
/// write_cursor を Acquire で読むことで、Writer が書いたデータが見えることを保証する
fn ring_read(mmap: &MmapMut, write_cursor: &AtomicU64, read_cursor: &AtomicU64, buf: &mut [u8]) -> usize {
    // データが来るまで待つ
    let rc = loop {
        let wc = write_cursor.load(Ordering::Acquire);
        let rc = read_cursor.load(Ordering::Relaxed);
        if wc - rc >= MSG_HEADER_SIZE as u64 {
            break rc;
        }
        hint::spin_loop();
    };

    let ring_base = mmap.as_ptr();

    // length header を読む
    let mut len_bytes = [0u8; 4];
    for (i, b) in len_bytes.iter_mut().enumerate() {
        let pos = HEADER_SIZE + ((rc as usize + i) % RING_SIZE);
        *b = unsafe { ring_base.add(pos).read_volatile() };
    }
    let data_len = u32::from_le_bytes(len_bytes) as usize;

    // payload が全部書かれるまで待つ
    loop {
        let wc = write_cursor.load(Ordering::Acquire);
        if wc - rc >= (MSG_HEADER_SIZE + data_len) as u64 {
            break;
        }
        hint::spin_loop();
    }

    // payload を読む
    let read_len = data_len.min(buf.len());
    for (i, b) in buf[..read_len].iter_mut().enumerate() {
        let pos = HEADER_SIZE + ((rc as usize + MSG_HEADER_SIZE + i) % RING_SIZE);
        *b = unsafe { ring_base.add(pos).read_volatile() };
    }

    // カーソルを進める
    read_cursor.store(rc + (MSG_HEADER_SIZE + data_len) as u64, Ordering::Release);

    read_len
}

fn main() {
    let role = std::env::args().nth(1).unwrap_or_default();
    match role.as_str() {
        "server" => run_server(),
        "client" => run_client(),
        _ => eprintln!("Usage: shm-playground <server|client>"),
    }
}

fn run_server() {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(true)
        .open(SHM_PATH)
        .expect("Failed to create shm file");
    file.set_len(SHM_SIZE as u64).expect("Failed to set size");

    let mmap = unsafe { MmapMut::map_mut(&file).expect("Failed to mmap") };
    let write_cursor = unsafe { atomic_at(&mmap, 0) };
    let read_cursor = unsafe { atomic_at(&mmap, 8) };

    write_cursor.store(0, Ordering::Release);
    read_cursor.store(0, Ordering::Release);

    println!("[server] Ring buffer ready. size={} bytes", RING_SIZE);
    thread::sleep(Duration::from_millis(300));

    // さまざまなサイズのメッセージを送る
    let messages: Vec<String> = vec![
        "Hello".into(),
        "World".into(),
        "This is a longer message to test wrapping".into(),
        "x".repeat(200), // リングの wrap を発生させる
        "Short".into(),
        "Done!".into(),
    ];

    for (i, msg) in messages.iter().enumerate() {
        ring_write(&mmap, write_cursor, read_cursor, msg.as_bytes());
        let wc = write_cursor.load(Ordering::Relaxed);
        println!(
            "[server] Sent msg {}: \"{}\" ({} bytes, cursor={})",
            i,
            if msg.len() > 30 {
                format!("{}...", &msg[..30])
            } else {
                msg.clone()
            },
            msg.len(),
            wc
        );
    }

    // client が全部読むのを待つ
    loop {
        let wc = write_cursor.load(Ordering::Relaxed);
        let rc = read_cursor.load(Ordering::Acquire);
        if rc >= wc {
            break;
        }
        hint::spin_loop();
    }

    println!("[server] Done.");
    let _ = std::fs::remove_file(SHM_PATH);
}

fn run_client() {
    while !Path::new(SHM_PATH).exists() {
        thread::sleep(Duration::from_millis(10));
    }

    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(SHM_PATH)
        .expect("Failed to open shm file");
    let mmap = unsafe { MmapMut::map_mut(&file).expect("Failed to mmap") };
    let write_cursor = unsafe { atomic_at(&mmap, 0) };
    let read_cursor = unsafe { atomic_at(&mmap, 8) };

    println!("[client] Connected to ring buffer.");

    let mut buf = vec![0u8; 512];
    for i in 0..6 {
        let n = ring_read(&mmap, write_cursor, read_cursor, &mut buf);
        let text = String::from_utf8_lossy(&buf[..n]);
        let rc = read_cursor.load(Ordering::Relaxed);
        println!(
            "[client] Recv msg {}: \"{}\" ({} bytes, cursor={})",
            i,
            if text.len() > 30 {
                format!("{}...", &text[..30])
            } else {
                text.to_string()
            },
            n,
            rc
        );
    }

    println!("[client] Done.");
}
