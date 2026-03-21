use clap::Parser;
use std::time::{Duration, Instant};

use ipc_bench::transport::named_pipe::NamedPipeTransport;
use ipc_bench::transport::shared_mem::*;
use ipc_bench::transport::tcp_socket::TcpSocketTransport;
use ipc_bench::transport::unix_socket::UnixSocketTransport;
use ipc_bench::transport::websocket::WebSocketTransport;
use ipc_bench::transport::{Role, Transport};

#[derive(Parser)]
#[command(name = "ipc-client")]
struct Args {
    /// トランスポート種別
    #[arg(short, long, default_value = "unix_socket")]
    transport: String,

    /// チャネル名（ソケットパス等の生成に使用）
    #[arg(short, long, default_value = "bench")]
    name: String,

    /// 送信メッセージサイズ (bytes)
    #[arg(short, long, default_value_t = 64)]
    size: usize,

    /// 1ラウンドあたりの送信回数
    #[arg(short, long, default_value_t = 1000)]
    count: usize,

    /// 計測ラウンド数 (中央値を採用)
    #[arg(short, long, default_value_t = 5)]
    rounds: usize,

    /// ウォームアップ回数 (計測前に捨てる)
    #[arg(short, long, default_value_t = 100)]
    warmup: usize,
}

/// 1ラウンドの結果
struct RoundResult {
    latencies: Vec<Duration>,
    total_elapsed: Duration,
}

/// 1ラウンド分の PingPong を実行
fn run_one_round<T: Transport>(
    transport: &mut T,
    send_buf: &[u8],
    recv_buf: &mut [u8],
    count: usize,
) -> RoundResult {
    let mut latencies = Vec::with_capacity(count);
    let total_start = Instant::now();

    for _ in 0..count {
        let start = Instant::now();
        transport.send(send_buf).expect("Failed to send");
        let n = transport.recv(recv_buf).expect("Failed to recv");
        let elapsed = start.elapsed();
        latencies.push(elapsed);
        assert_eq!(n, send_buf.len(), "Echo size mismatch");
    }

    let total_elapsed = total_start.elapsed();
    RoundResult {
        latencies,
        total_elapsed,
    }
}

fn run_ping_pong<T: Transport>(
    name: &str,
    msg_size: usize,
    count: usize,
    rounds: usize,
    warmup: usize,
) {
    println!("[client] Connecting...");
    let mut transport = T::open(name, Role::Client).expect("Failed to open transport");
    println!(
        "[client] Connected. transport={}, size={}, count={}, rounds={}, warmup={}",
        T::transport_name(),
        msg_size,
        count,
        rounds,
        warmup
    );

    let send_buf = vec![0xABu8; msg_size];
    let mut recv_buf = vec![0u8; msg_size];

    // --- ウォームアップ: キャッシュ・分岐予測を温める ---
    if warmup > 0 {
        for _ in 0..warmup {
            transport.send(&send_buf).expect("Failed to send");
            transport.recv(&mut recv_buf).expect("Failed to recv");
        }
    }

    // --- 複数ラウンド実行 ---
    let mut round_results: Vec<RoundResult> = Vec::with_capacity(rounds);
    for r in 0..rounds {
        let result = run_one_round::<T>(&mut transport, &send_buf, &mut recv_buf, count);
        let p50 = {
            let mut sorted = result.latencies.clone();
            sorted.sort();
            sorted[count / 2]
        };
        println!(
            "  Round {}/{}: p50={:.2?}, total={:.2?}",
            r + 1,
            rounds,
            p50,
            result.total_elapsed
        );
        round_results.push(result);
    }

    // --- 各ラウンドのスループットで中央ラウンドを選ぶ ---
    let total_bytes = msg_size as u64 * count as u64 * 2;
    let mut throughputs: Vec<(usize, f64)> = round_results
        .iter()
        .enumerate()
        .map(|(i, r)| {
            let mbs = total_bytes as f64 / (1024.0 * 1024.0) / r.total_elapsed.as_secs_f64();
            (i, mbs)
        })
        .collect();
    throughputs.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap());
    let median_idx = throughputs[rounds / 2].0;
    let median_result = &round_results[median_idx];

    // --- 中央ラウンドの結果を表示 ---
    let mut latencies = median_result.latencies.clone();
    latencies.sort();
    let p50 = latencies[count / 2];
    let p95 = latencies[count * 95 / 100];
    let p99 = latencies[count * 99 / 100];
    let min = latencies[0];
    let max = latencies[count - 1];
    let throughput_mbs =
        total_bytes as f64 / (1024.0 * 1024.0) / median_result.total_elapsed.as_secs_f64();

    println!();
    println!("=== Results: {} (median of {} rounds) ===", T::transport_name(), rounds);
    println!("  Messages:   {}", count);
    println!("  Msg size:   {} bytes", msg_size);
    println!("  Warmup:     {}", warmup);
    println!("  Total time: {:.2?}", median_result.total_elapsed);
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
    let (name, size, count, rounds, warmup) =
        (&args.name, args.size, args.count, args.rounds, args.warmup);

    match args.transport.as_str() {
        "unix_socket" => run_ping_pong::<UnixSocketTransport>(name, size, count, rounds, warmup),
        "tcp_socket" => run_ping_pong::<TcpSocketTransport>(name, size, count, rounds, warmup),
        "websocket" => run_ping_pong::<WebSocketTransport>(name, size, count, rounds, warmup),
        "named_pipe" => run_ping_pong::<NamedPipeTransport>(name, size, count, rounds, warmup),
        "shared_mem" => run_ping_pong::<SharedMemPadded>(name, size, count, rounds, warmup),
        "shared_mem_compact" => run_ping_pong::<SharedMemCompact>(name, size, count, rounds, warmup),
        "shared_mem_inline" => run_ping_pong::<SharedMemInline>(name, size, count, rounds, warmup),
        "shared_mem_inline512" => run_ping_pong::<SharedMemInline512>(name, size, count, rounds, warmup),
        "shared_mem_inline1k" => run_ping_pong::<SharedMemInline1k>(name, size, count, rounds, warmup),
        "shared_mem_inline2k" => run_ping_pong::<SharedMemInline2k>(name, size, count, rounds, warmup),
        "shared_mem_inline4k" => run_ping_pong::<SharedMemInline4k>(name, size, count, rounds, warmup),
        "shared_mem_inline8k" => run_ping_pong::<SharedMemInline8k>(name, size, count, rounds, warmup),
        "shared_mem_uninit" => run_ping_pong::<SharedMemUninit>(name, size, count, rounds, warmup),
        "shared_mem_uninit512" => run_ping_pong::<SharedMemUninit512>(name, size, count, rounds, warmup),
        "shared_mem_uninit1k" => run_ping_pong::<SharedMemUninit1k>(name, size, count, rounds, warmup),
        "shared_mem_uninit2k" => run_ping_pong::<SharedMemUninit2k>(name, size, count, rounds, warmup),
        "shared_mem_uninit4k" => run_ping_pong::<SharedMemUninit4k>(name, size, count, rounds, warmup),
        "shared_mem_uninit8k" => run_ping_pong::<SharedMemUninit8k>(name, size, count, rounds, warmup),
        other => {
            eprintln!("Unknown transport: {}", other);
            std::process::exit(1);
        }
    }
}
