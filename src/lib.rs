pub mod transport;

/// CLI の transport 名から対応する型で関数を呼び出す dispatch マクロ
///
/// 新しい Transport / SharedMem バリエーションを追加する際、ここに1行足すだけでよい。
/// server / client の match 分岐が自動的に揃う。
///
/// プラットフォーム固有のトランスポートは #[cfg] でゲートされる。
#[macro_export]
macro_rules! dispatch_transport {
    ($transport_str:expr, $func:ident ( $($arg:expr),* $(,)? )) => {
        match $transport_str {
            // --- クロスプラットフォーム ---
            "tcp_socket"  => $func::<$crate::transport::tcp_socket::TcpSocketTransport>($($arg),*),
            "websocket"   => $func::<$crate::transport::websocket::WebSocketTransport>($($arg),*),

            // --- Unix 固有 ---
            #[cfg(unix)]
            "unix_socket" => $func::<$crate::transport::unix_socket::UnixSocketTransport>($($arg),*),
            #[cfg(unix)]
            "named_pipe"  => $func::<$crate::transport::named_pipe::NamedPipeTransport>($($arg),*),

            // --- Windows 固有 ---
            #[cfg(windows)]
            "named_pipe"  => $func::<$crate::transport::named_pipe_win::WinNamedPipeTransport>($($arg),*),

            // --- SharedMem: 検証マトリクス (HeaderLayout × FrameStrategy) ---
            "shared_mem"                  => $func::<$crate::transport::shared_mem::SharedMemPadded>($($arg),*),
            "shared_mem_compact"          => $func::<$crate::transport::shared_mem::SharedMemCompact>($($arg),*),
            "shared_mem_uninit1k"         => $func::<$crate::transport::shared_mem::SharedMemUninit1k>($($arg),*),
            "shared_mem_compact_uninit1k" => $func::<$crate::transport::shared_mem::SharedMemCompactUninit1k>($($arg),*),

            // --- SharedMem: 閾値スイープ用 (Padded固定) ---
            "shared_mem_inline"      => $func::<$crate::transport::shared_mem::SharedMemInline>($($arg),*),
            "shared_mem_inline512"   => $func::<$crate::transport::shared_mem::SharedMemInline512>($($arg),*),
            "shared_mem_inline1k"    => $func::<$crate::transport::shared_mem::SharedMemInline1k>($($arg),*),
            "shared_mem_inline2k"    => $func::<$crate::transport::shared_mem::SharedMemInline2k>($($arg),*),
            "shared_mem_inline4k"    => $func::<$crate::transport::shared_mem::SharedMemInline4k>($($arg),*),
            "shared_mem_inline8k"    => $func::<$crate::transport::shared_mem::SharedMemInline8k>($($arg),*),
            "shared_mem_uninit"      => $func::<$crate::transport::shared_mem::SharedMemUninit>($($arg),*),
            "shared_mem_uninit512"   => $func::<$crate::transport::shared_mem::SharedMemUninit512>($($arg),*),
            "shared_mem_uninit2k"    => $func::<$crate::transport::shared_mem::SharedMemUninit2k>($($arg),*),
            "shared_mem_uninit4k"    => $func::<$crate::transport::shared_mem::SharedMemUninit4k>($($arg),*),
            "shared_mem_uninit8k"    => $func::<$crate::transport::shared_mem::SharedMemUninit8k>($($arg),*),

            other => {
                eprintln!("Unknown transport: {}", other);
                std::process::exit(1);
            }
        }
    };
}
