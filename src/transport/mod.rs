pub mod named_pipe;
pub mod shared_mem;
pub mod tcp_socket;
pub mod unix_socket;
pub mod websocket;

use std::io;

/// 通信の役割
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    Server,
    Client,
}

/// IPC トランスポートの共通インターフェース
pub trait Transport: Send + Sized {
    /// トランスポートを開く。name は論理チャネル名（ソケットパス等に使う）
    fn open(name: &str, role: Role) -> io::Result<Self>;

    /// buf の全バイトを送信する
    fn send(&mut self, buf: &[u8]) -> io::Result<()>;

    /// メッセージを1つ受信して buf に読み込む。読めたバイト数を返す
    fn recv(&mut self, buf: &mut [u8]) -> io::Result<usize>;

    /// OS リソースを解放する（ソケットファイル削除など）
    fn cleanup(name: &str) -> io::Result<()>;

    /// レポート用の表示名
    fn transport_name() -> &'static str;
}
