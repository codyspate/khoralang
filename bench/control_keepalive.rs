// The same server, answering many requests on one connection. Everything else
// is identical, so the difference is connection reuse and nothing else.
use std::io::{Read, Write};
use std::net::TcpListener;

fn main() {
    let port: u16 = std::env::args().nth(1).unwrap().parse().unwrap();
    let listener = TcpListener::bind(("127.0.0.1", port)).unwrap();
    println!("listening");
    for stream in listener.incoming() {
        let Ok(mut s) = stream else { continue };
        s.set_nodelay(true).ok();
        std::thread::spawn(move || {
            let mut buf = [0u8; 8192];
            loop {
                match s.read(&mut buf) {
                    Ok(0) | Err(_) => return,
                    Ok(_) => {}
                }
                let body = b"{\"status\":\"ok\"}";
                let head = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n",
                    body.len()
                );
                if s.write_all(head.as_bytes()).is_err() || s.write_all(body).is_err() {
                    return;
                }
            }
        });
    }
}
