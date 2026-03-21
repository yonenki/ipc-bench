use std::io;
use std::net::{TcpListener, TcpStream};
use std::thread;
use std::time::Duration;

use tungstenite::protocol::Message;
use tungstenite::{accept, client};

use super::{Role, Transport};

/// WebSocket (localhost) によるトランスポート
pub struct WebSocketTransport {
    ws: tungstenite::WebSocket<TcpStream>,
}

impl WebSocketTransport {
    fn port(name: &str) -> u16 {
        let hash: u32 = name
            .bytes()
            .fold(0u32, |acc, b| acc.wrapping_mul(37).wrapping_add(b as u32));
        11000 + (hash % 50000) as u16
    }
}

impl Transport for WebSocketTransport {
    fn open(name: &str, role: Role) -> io::Result<Self> {
        let port = Self::port(name);
        let addr = format!("127.0.0.1:{}", port);

        let ws = match role {
            Role::Server => {
                let listener = TcpListener::bind(&addr)?;
                let (stream, _) = listener.accept()?;
                stream.set_nodelay(true)?;
                accept(stream).map_err(|e| io::Error::new(io::ErrorKind::Other, e))?
            }
            Role::Client => {
                let url = format!("ws://{}", addr);
                let mut retries = 0;
                loop {
                    match TcpStream::connect(&addr) {
                        Ok(stream) => {
                            stream.set_nodelay(true)?;
                            let (ws, _) = client(&url, stream)
                                .map_err(|e| io::Error::new(io::ErrorKind::Other, e))?;
                            break ws;
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

        Ok(Self { ws })
    }

    fn send(&mut self, buf: &[u8]) -> io::Result<()> {
        self.ws
            .send(Message::Binary(buf.to_vec().into()))
            .map_err(|e| io::Error::new(io::ErrorKind::Other, e))
    }

    fn recv(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        loop {
            let msg = self
                .ws
                .read()
                .map_err(|e| io::Error::new(io::ErrorKind::Other, e))?;

            match msg {
                Message::Binary(data) => {
                    let len = data.len().min(buf.len());
                    buf[..len].copy_from_slice(&data[..len]);
                    return Ok(len);
                }
                // Ping/Pong/Close 等は無視して次を読む
                _ => continue,
            }
        }
    }

    fn cleanup(_name: &str) -> io::Result<()> {
        Ok(())
    }

    fn transport_name() -> &'static str {
        "WebSocket"
    }
}
