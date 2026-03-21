use clap::Parser;
use std::time::Instant;

use ipc_bench::transport::named_pipe::NamedPipeTransport;
use ipc_bench::transport::shared_mem::SharedMemTransport;
use ipc_bench::transport::tcp_socket::TcpSocketTransport;
use ipc_bench::transport::unix_socket::UnixSocketTransport;
use ipc_bench::transport::websocket::WebSocketTransport;
use ipc_bench::transport::{Role, Transport};

#[derive(Parser)]
#[command(name = "ipc-client")]
struct Args {
    /// トランスポート種別: unix_socket, tcp_socket, websocket, named_pipe, shared_mem
    #[arg(short, long, default_value = "unix_socket")]
    transport: String,

    /// チャネル名（ソケットパス等の生成に使用）
    #[arg(short, long, default_value = "bench")]
    name: String,

    /// 送信メッセージサイズ (bytes)
    #[arg(short, long, default_value_t = 64)]
    size: usize,

    /// 送信回数
    #[arg(short, long, default_value_t = 1000)]
    count: usize,
}

fn run_ping_pong<T: Transport>(name: &str, msg_size: usize, count: usize) {
    println!("[client] Connecting...");
    let mut transport = T::open(name, Role::Client).expect("Failed to open transport");
    println!(
        "[client] Connected. transport={}, size={}, count={}",
        T::transport_name(),
        msg_size,
        count
    );

    let send_buf = vec![0xABu8; msg_size];
    let mut recv_buf = vec![0u8; msg_size];
    let mut latencies = Vec::with_capacity(count);

    let total_start = Instant::now();

    for _ in 0..count {
        let start = Instant::now();
        transport.send(&send_buf).expect("Failed to send");
        let n = transport.recv(&mut recv_buf).expect("Failed to recv");
        let elapsed = start.elapsed();
        latencies.push(elapsed);
        assert_eq!(n, msg_size, "Echo size mismatch");
    }

    let total_elapsed = total_start.elapsed();

    // --- 結果表示 ---
    latencies.sort();
    let p50 = latencies[count / 2];
    let p95 = latencies[count * 95 / 100];
    let p99 = latencies[count * 99 / 100];
    let min = latencies[0];
    let max = latencies[count - 1];
    let total_bytes = msg_size as u64 * count as u64 * 2; // send + recv
    let throughput_mbs =
        total_bytes as f64 / (1024.0 * 1024.0) / total_elapsed.as_secs_f64();

    println!();
    println!("=== Results: {} ===", T::transport_name());
    println!("  Messages:   {}", count);
    println!("  Msg size:   {} bytes", msg_size);
    println!("  Total time: {:.2?}", total_elapsed);
    println!("  Latency (round-trip):");
    println!("    min: {:>10.2?}", min);
    println!("    p50: {:>10.2?}", p50);
    println!("    p95: {:>10.2?}", p95);
    println!("    p99: {:>10.2?}", p99);
    println!("    max: {:>10.2?}", max);
    println!("  Throughput:  {:.2} MB/s", throughput_mbs);
}

fn main() {
    let args = Args::parse();

    match args.transport.as_str() {
        "unix_socket" => run_ping_pong::<UnixSocketTransport>(&args.name, args.size, args.count),
        "tcp_socket" => run_ping_pong::<TcpSocketTransport>(&args.name, args.size, args.count),
        "websocket" => run_ping_pong::<WebSocketTransport>(&args.name, args.size, args.count),
        "named_pipe" => run_ping_pong::<NamedPipeTransport>(&args.name, args.size, args.count),
        "shared_mem" => run_ping_pong::<SharedMemTransport>(&args.name, args.size, args.count),
        other => {
            eprintln!("Unknown transport: {}", other);
            std::process::exit(1);
        }
    }
}
