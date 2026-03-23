use std::fs::File;
use std::io::{self, BufWriter, Read, Write};
use std::os::windows::io::FromRawHandle;
use std::thread;
use std::time::Duration;

use super::{Role, Transport};

/// Windows Named Pipe によるトランスポート
///
/// Windows の Named Pipe は双方向なので、Unix FIFO (2本必要) と違い1本で済む。
/// Server が CreateNamedPipe で作成し ConnectNamedPipe で接続待ち、
/// Client が CreateFile で接続する。
///
/// send は BufWriter で length + payload を1回の flush にまとめる (syscall 削減)。
pub struct WinNamedPipeTransport {
    pipe: File,
}

impl WinNamedPipeTransport {
    fn pipe_name(name: &str) -> String {
        format!(r"\\.\pipe\ipc_bench_{}", name)
    }
}

impl Transport for WinNamedPipeTransport {
    fn open(name: &str, role: Role) -> io::Result<Self> {
        use INVALID_HANDLE_VALUE;
        use windows_sys::Win32::Storage::FileSystem::{
            CreateFileW, GENERIC_READ, GENERIC_WRITE, OPEN_EXISTING,
        };
        use windows_sys::Win32::System::Pipes::*;

        let pipe_name = Self::pipe_name(name);
        let wide_name: Vec<u16> = pipe_name.encode_utf16().chain(std::iter::once(0)).collect();

        match role {
            Role::Server => {
                let handle = unsafe {
                    CreateNamedPipeW(
                        wide_name.as_ptr(),
                        PIPE_ACCESS_DUPLEX,
                        PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT,
                        1,          // max instances
                        64 * 1024,  // out buffer size
                        64 * 1024,  // in buffer size
                        0,          // default timeout
                        std::ptr::null(), // security attributes
                    )
                };
                if handle == INVALID_HANDLE_VALUE {
                    return Err(io::Error::last_os_error());
                }

                // クライアントの接続を待つ
                let ok = unsafe { ConnectNamedPipe(handle, std::ptr::null_mut()) };
                if ok == 0 {
                    let err = io::Error::last_os_error();
                    // ERROR_PIPE_CONNECTED は既に接続済み (正常)
                    if err.raw_os_error() != Some(535) {
                        return Err(err);
                    }
                }

                let pipe = unsafe { File::from_raw_handle(handle as _) };
                Ok(Self { pipe })
            }
            Role::Client => {
                let mut retries = 0;
                loop {
                    let handle = unsafe {
                        CreateFileW(
                            wide_name.as_ptr(),
                            GENERIC_READ | GENERIC_WRITE,
                            0,
                            std::ptr::null(),
                            OPEN_EXISTING,
                            0,
                            0, // template file handle (unused)
                        )
                    };
                    if handle != INVALID_HANDLE_VALUE {
                        let pipe = unsafe { File::from_raw_handle(handle as _) };
                        return Ok(Self { pipe });
                    }
                    if retries >= 100 {
                        return Err(io::Error::last_os_error());
                    }
                    retries += 1;
                    thread::sleep(Duration::from_millis(10));
                }
            }
        }
    }

    fn send(&mut self, buf: &[u8]) -> io::Result<()> {
        // BufWriter で length + payload を1回の flush にまとめる
        let len = buf.len() as u32;
        let mut writer = BufWriter::new(&mut self.pipe);
        writer.write_all(&len.to_le_bytes())?;
        writer.write_all(buf)?;
        writer.flush()?;
        Ok(())
    }

    fn recv(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let mut len_buf = [0u8; 4];
        self.pipe.read_exact(&mut len_buf)?;
        let len = u32::from_le_bytes(len_buf) as usize;

        let read_len = len.min(buf.len());
        self.pipe.read_exact(&mut buf[..read_len])?;

        if len > read_len {
            let mut discard = vec![0u8; len - read_len];
            self.pipe.read_exact(&mut discard)?;
        }

        Ok(read_len)
    }

    fn cleanup(_name: &str) -> io::Result<()> {
        // Windows Named Pipe はハンドルを閉じれば自動的にクリーンアップされる
        Ok(())
    }

    fn transport_name() -> &'static str {
        "NamedPipe(Win)"
    }
}
