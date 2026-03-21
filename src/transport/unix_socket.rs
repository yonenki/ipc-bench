use std::io::{self, Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::thread;
use std::time::Duration;

use super::{Role, Transport};

/// Unix domain socket (SOCK_STREAM) によるトランスポート
pub struct UnixSocketTransport {
    stream: UnixStream,
}

impl UnixSocketTransport {
    /// チャネル名からソケットファイルパスを生成
    fn socket_path(name: &str) -> PathBuf {
        PathBuf::from(format!("/tmp/ipc_bench_{}.sock", name))
    }
}

impl Transport for UnixSocketTransport {
    fn open(name: &str, role: Role) -> io::Result<Self> {
        let path = Self::socket_path(name);

        let stream = match role {
            Role::Server => {
                // 既存ソケットファイルがあれば削除
                let _ = std::fs::remove_file(&path);
                let listener = UnixListener::bind(&path)?;
                // 1接続だけ accept
                let (stream, _addr) = listener.accept()?;
                stream
            }
            Role::Client => {
                // Server の bind を待つためリトライ
                let mut retries = 0;
                loop {
                    match UnixStream::connect(&path) {
                        Ok(stream) => break stream,
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
        // 4byte LE length prefix + payload
        let len = buf.len() as u32;
        self.stream.write_all(&len.to_le_bytes())?;
        self.stream.write_all(buf)?;
        Ok(())
    }

    fn recv(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        // 4byte length を読む
        let mut len_buf = [0u8; 4];
        self.stream.read_exact(&mut len_buf)?;
        let len = u32::from_le_bytes(len_buf) as usize;

        // payload を読む (buf が足りない場合は buf サイズ分だけ読む)
        let read_len = len.min(buf.len());
        self.stream.read_exact(&mut buf[..read_len])?;

        // buf より大きいデータが来た場合は残りを捨てる
        if len > read_len {
            let mut discard = vec![0u8; len - read_len];
            self.stream.read_exact(&mut discard)?;
        }

        Ok(read_len)
    }

    fn cleanup(name: &str) -> io::Result<()> {
        let path = Self::socket_path(name);
        match std::fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e),
        }
    }

    fn transport_name() -> &'static str {
        "UnixSocket"
    }
}
