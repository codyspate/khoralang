// A plain Rust thread-per-connection server: the same shape as Khora's, so the
// difference between them is the language and its runtime rather than the
// architecture. Deliberately not axum — that would compare two things at once.
use std::io::{Read, Write};
use std::net::TcpListener;

fn main() {
    let port: u16 = std::env::args().nth(1).unwrap().parse().unwrap();
    let listener = TcpListener::bind(("127.0.0.1", port)).unwrap();
    println!("listening");
    for stream in listener.incoming() {
        let Ok(mut s) = stream else { continue };
        std::thread::spawn(move || {
            let mut buf = [0u8; 8192];
            let _ = s.read(&mut buf);
            let body = b"{\"status\":\"ok\"}";
            let head = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            let _ = s.write_all(head.as_bytes());
            let _ = s.write_all(body);
        });
    }
}
