use std::io::{self, BufWriter, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::thread;
use std::time::Duration;

use super::{Role, Transport};

/// TCP Socket (localhost) によるトランスポート
///
/// send 実装には2つの方式がある:
/// - v1 (旧): write_all を2回（length + payload）→ syscall が2回発生しオーバーヘッド大
/// - v2 (現): BufWriter で length + payload を1回の flush にまとめる → syscall 1回
///
/// WebSocket (tungstenite) が TCP 生より速かった原因は、tungstenite が内部で
/// フレームヘッダ + payload を1回の write にまとめていたため。
/// この改善で同等以上の性能が出るはず。
pub struct TcpSocketTransport {
    stream: TcpStream,
}

impl TcpSocketTransport {
    /// チャネル名からポート番号を決定する (名前のハッシュで 10000-60000 の範囲)
    fn port(name: &str) -> u16 {
        let hash: u32 = name
            .bytes()
            .fold(0u32, |acc, b| acc.wrapping_mul(31).wrapping_add(b as u32));
        10000 + (hash % 50000) as u16
    }
}

impl Transport for TcpSocketTransport {
    fn open(name: &str, role: Role) -> io::Result<Self> {
        let port = Self::port(name);
        let addr = format!("127.0.0.1:{}", port);

        let stream = match role {
            Role::Server => {
                let listener = TcpListener::bind(&addr)?;
                let (stream, _) = listener.accept()?;
                stream.set_nodelay(true)?;
                stream
            }
            Role::Client => {
                let mut retries = 0;
                loop {
                    match TcpStream::connect(&addr) {
                        Ok(stream) => {
                            stream.set_nodelay(true)?;
                            break stream;
                        }
                        Err(_) if retries < 100 => {
                            retries += 1;
                            thread::sleep(Duration::from_millis(10));
                        }
                        Err(e) => return Err(e),
                    }
                }
            }
        };

        Ok(Self { stream })
    }

    fn send(&mut self, buf: &[u8]) -> io::Result<()> {
        // --- v1: 素朴な実装 (syscall 2回) ---
        // length と payload を別々に write_all するため、set_nodelay(true) 環境では
        // 各 write_all が即座に TCP セグメントとして送出され、syscall オーバーヘッドが2倍になる。
        // WebSocket (tungstenite) より遅かった原因。
        //
        // let len = buf.len() as u32;
        // self.stream.write_all(&len.to_le_bytes())?;
        // self.stream.write_all(buf)?;

        // --- v2: BufWriter で1回にまとめる (syscall 1回) ---
        // BufWriter が length + payload をバッファに溜め、flush で1回の write syscall にまとめる。
        let len = buf.len() as u32;
        let mut writer = BufWriter::new(&mut self.stream);
        writer.write_all(&len.to_le_bytes())?;
        writer.write_all(buf)?;
        writer.flush()?;
        Ok(())
    }

    fn recv(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let mut len_buf = [0u8; 4];
        self.stream.read_exact(&mut len_buf)?;
        let len = u32::from_le_bytes(len_buf) as usize;

        let read_len = len.min(buf.len());
        self.stream.read_exact(&mut buf[..read_len])?;

        if len > read_len {
            let mut discard = vec![0u8; len - read_len];
            self.stream.read_exact(&mut discard)?;
        }

        Ok(read_len)
    }

    fn cleanup(_name: &str) -> io::Result<()> {
        // TCP はファイルを作らないので何もしない
        Ok(())
    }

    fn transport_name() -> &'static str {
        "TcpSocket"
    }
}
