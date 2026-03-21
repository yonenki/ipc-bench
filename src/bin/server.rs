use clap::Parser;
use ipc_bench::transport::named_pipe::NamedPipeTransport;
use ipc_bench::transport::shared_mem::SharedMemTransport;
use ipc_bench::transport::tcp_socket::TcpSocketTransport;
use ipc_bench::transport::unix_socket::UnixSocketTransport;
use ipc_bench::transport::websocket::WebSocketTransport;
use ipc_bench::transport::{Role, Transport};

#[derive(Parser)]
#[command(name = "ipc-server")]
struct Args {
    /// トランスポート種別: unix_socket, tcp_socket, websocket, named_pipe, shared_mem
    #[arg(short, long, default_value = "unix_socket")]
    transport: String,

    /// チャネル名（ソケットパス等の生成に使用）
    #[arg(short, long, default_value = "bench")]
    name: String,

    /// 受信するメッセージ数 (0 = 無制限)
    #[arg(short, long, default_value_t = 0)]
    count: usize,
}

fn run_echo_server<T: Transport>(name: &str, count: usize) {
    println!("[server] Waiting for connection...");
    let mut transport = T::open(name, Role::Server).expect("Failed to open transport");
    println!("[server] Connected. transport={}", T::transport_name());

    let mut buf = vec![0u8; 16 * 1024 * 1024]; // 16MB バッファ
    let mut i = 0;
    loop {
        if count > 0 && i >= count {
            break;
        }
        match transport.recv(&mut buf) {
            Ok(n) => {
                // 受信データをそのままエコー
                transport.send(&buf[..n]).expect("Failed to send echo");
                i += 1;
            }
            Err(e) => {
                eprintln!("[server] recv error: {}", e);
                break;
            }
        }
    }
    println!("[server] Done. {} messages echoed.", i);
    let _ = T::cleanup(name);
}

fn main() {
    let args = Args::parse();

    match args.transport.as_str() {
        "unix_socket" => run_echo_server::<UnixSocketTransport>(&args.name, args.count),
        "tcp_socket" => run_echo_server::<TcpSocketTransport>(&args.name, args.count),
        "websocket" => run_echo_server::<WebSocketTransport>(&args.name, args.count),
        "named_pipe" => run_echo_server::<NamedPipeTransport>(&args.name, args.count),
        "shared_mem" => run_echo_server::<SharedMemTransport>(&args.name, args.count),
        other => {
            eprintln!("Unknown transport: {}", other);
            std::process::exit(1);
        }
    }
}
