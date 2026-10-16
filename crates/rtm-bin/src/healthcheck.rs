// SPDX-License-Identifier: AGPL-3.0-or-later
//! `rtm healthcheck ADDR`: a container healthcheck. The images have no shell or curl,
//! so this asks `GET /healthz` of a local `rtm` process itself and exits 0 on a 200.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::process::ExitCode;
use std::time::Duration;

const TIMEOUT: Duration = Duration::from_secs(3);

pub fn run(addr: &str) -> ExitCode {
    match check(addr) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("unhealthy: {e}");
            ExitCode::FAILURE
        }
    }
}

fn check(addr: &str) -> Result<(), String> {
    let addr: SocketAddr = addr
        .parse()
        .map_err(|_| format!("{addr} is not an address and port, e.g. 127.0.0.1:8080"))?;
    let mut stream = TcpStream::connect_timeout(&addr, TIMEOUT).map_err(|e| e.to_string())?;
    stream
        .set_read_timeout(Some(TIMEOUT))
        .map_err(|e| e.to_string())?;
    stream
        .set_write_timeout(Some(TIMEOUT))
        .map_err(|e| e.to_string())?;
    let request = format!("GET /healthz HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n\r\n");
    stream
        .write_all(request.as_bytes())
        .map_err(|e| e.to_string())?;
    // Only the status line matters.
    let mut head = Vec::new();
    stream
        .take(64)
        .read_to_end(&mut head)
        .map_err(|e| e.to_string())?;
    let status = String::from_utf8_lossy(&head);
    let status = status.lines().next().unwrap_or_default();
    if status.starts_with("HTTP/1.1 200 ") || status.starts_with("HTTP/1.0 200 ") {
        Ok(())
    } else {
        Err(format!("/healthz answered {status:?}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    fn serve_once(response: &'static str) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        std::thread::spawn(move || {
            let (mut s, _) = listener.accept().unwrap();
            let mut request = Vec::new();
            let mut byte = [0u8; 1];
            while !request.ends_with(b"\r\n\r\n") && s.read(&mut byte).unwrap_or(0) == 1 {
                request.push(byte[0]);
            }
            let _ = s.write_all(response.as_bytes());
        });
        addr
    }

    #[test]
    fn healthy_only_on_a_200() {
        let ok = serve_once("HTTP/1.1 200 OK\r\ncontent-length: 2\r\n\r\nok");
        assert_eq!(check(&ok), Ok(()));
        let down = serve_once("HTTP/1.1 503 Service Unavailable\r\n\r\n");
        let e = check(&down).unwrap_err();
        assert!(e.contains("503"), "{e}");
        assert!(check("not an address").is_err());
        // Nothing listening.
        let closed = TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap();
        assert!(check(&closed.to_string()).is_err());
    }
}
