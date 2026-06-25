use std::env;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::thread;

fn main() -> std::io::Result<()> {
    let tunnel = env::var("PZ_TUNNEL").unwrap_or_else(|_| "<unset>".to_string());
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let port = listener.local_addr()?.port();

    eprintln!("[rust-server] bound to 127.0.0.1:{port} (ephemeral)");
    eprintln!("[rust-server] PZ_TUNNEL={tunnel}");
    eprintln!("[rust-server] direct check: curl http://127.0.0.1:{port}/");
    if tunnel != "<unset>" {
        eprintln!("[rust-server] overlay check: curl http://{tunnel}/");
    }

    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                let tunnel = tunnel.clone();
                thread::spawn(move || {
                    if let Err(err) = handle_client(stream, &tunnel, port) {
                        eprintln!("[rust-server] request failed: {err}");
                    }
                });
            }
            Err(err) => eprintln!("[rust-server] accept failed: {err}"),
        }
    }

    Ok(())
}

fn handle_client(mut stream: TcpStream, tunnel: &str, port: u16) -> std::io::Result<()> {
    let mut buf = [0_u8; 1024];
    let _ = stream.read(&mut buf)?;

    let body = format!(
        "Hello from the Port Zero Rust local process example!\nPZ_TUNNEL={tunnel}\nserved on real port {port}\n"
    );
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        body.len(),
        body
    );

    stream.write_all(response.as_bytes())?;
    stream.flush()
}
