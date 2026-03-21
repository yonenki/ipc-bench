use std::fs::{File, OpenOptions};
use std::io::{self, BufWriter, Read, Write};
use std::path::PathBuf;
use std::thread;
use std::time::Duration;

use super::{Role, Transport};

/// Named Pipe (FIFO) によるトランスポート
///
/// FIFO は単方向なので、双方向通信には2本必要:
/// - s2c: server → client
/// - c2s: client → server
///
/// send は BufWriter で length + payload を1回の write にまとめている。
/// TCP で学んだ通り、素朴に write_all 2回だと syscall が倍になりレイテンシが悪化する。
pub struct NamedPipeTransport {
    writer: File,
    reader: File,
}

impl NamedPipeTransport {
    fn s2c_path(name: &str) -> PathBuf {
        PathBuf::from(format!("/tmp/ipc_bench_{}_s2c", name))
    }

    fn c2s_path(name: &str) -> PathBuf {
        PathBuf::from(format!("/tmp/ipc_bench_{}_c2s", name))
    }

    /// FIFO を作成する。既に存在する場合は削除してから作り直す。
    fn create_fifo(path: &PathBuf) -> io::Result<()> {
        let _ = std::fs::remove_file(path);
        let c_path = std::ffi::CString::new(path.to_str().unwrap()).unwrap();
        let ret = unsafe { libc::mkfifo(c_path.as_ptr(), 0o644) };
        if ret != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }
}

impl Transport for NamedPipeTransport {
    fn open(name: &str, role: Role) -> io::Result<Self> {
        let s2c = Self::s2c_path(name);
        let c2s = Self::c2s_path(name);

        match role {
            Role::Server => {
                // Server が FIFO を作成
                Self::create_fifo(&s2c)?;
                Self::create_fifo(&c2s)?;

                // open はブロックする（相手が開くまで待つ）
                // デッドロック防止: server は write 側を先に開く
                // (client は read 側を先に開く)
                let writer = OpenOptions::new().write(true).open(&s2c)?;
                let reader = OpenOptions::new().read(true).open(&c2s)?;

                Ok(Self { writer, reader })
            }
            Role::Client => {
                // FIFO が作られるのを待つ
                let mut retries = 0;
                loop {
                    if s2c.exists() && c2s.exists() {
                        break;
                    }
                    if retries >= 100 {
                        return Err(io::Error::new(
                            io::ErrorKind::TimedOut,
                            "FIFO not created by server",
                        ));
                    }
                    retries += 1;
                    thread::sleep(Duration::from_millis(10));
                }

                // デッドロック防止: client は read 側を先に開く
                let reader = OpenOptions::new().read(true).open(&s2c)?;
                let writer = OpenOptions::new().write(true).open(&c2s)?;

                Ok(Self { writer, reader })
            }
        }
    }

    fn send(&mut self, buf: &[u8]) -> io::Result<()> {
        // BufWriter で length + payload を1回の flush にまとめる (syscall 削減)
        let len = buf.len() as u32;
        let mut writer = BufWriter::new(&mut self.writer);
        writer.write_all(&len.to_le_bytes())?;
        writer.write_all(buf)?;
        writer.flush()?;
        Ok(())
    }

    fn recv(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let mut len_buf = [0u8; 4];
        self.reader.read_exact(&mut len_buf)?;
        let len = u32::from_le_bytes(len_buf) as usize;

        let read_len = len.min(buf.len());
        self.reader.read_exact(&mut buf[..read_len])?;

        if len > read_len {
            let mut discard = vec![0u8; len - read_len];
            self.reader.read_exact(&mut discard)?;
        }

        Ok(read_len)
    }

    fn cleanup(name: &str) -> io::Result<()> {
        let _ = std::fs::remove_file(Self::s2c_path(name));
        let _ = std::fs::remove_file(Self::c2s_path(name));
        Ok(())
    }

    fn transport_name() -> &'static str {
        "NamedPipe"
    }
}
