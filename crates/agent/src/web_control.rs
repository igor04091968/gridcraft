use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};

use gridcraft_mcp::control_auth;
use serde_json::{Value, json};
use tungstenite::{Message, WebSocket};

const MAX_LINE: u64 = 64 * 1024 * 1024;

pub fn main(args: &[String]) -> Result<(), String> {
    if let [option, path] = args
        && option == "--config"
    {
        return crate::remote::main(std::path::Path::new(path));
    }
    let mut listen = "127.0.0.1:8780".to_string();
    let mut port = "7979".to_string();
    let mut token = std::env::var("GRIDCRAFT_CONTROL_TOKEN").ok();
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        let (name, inline) = arg.split_once('=').map_or((arg.as_str(), None), |(a, b)| (a, Some(b.to_string())));
        let value = |it: &mut std::slice::Iter<String>| inline.clone().or_else(|| it.next().cloned()).ok_or_else(|| format!("{name} needs a value"));
        match name {
            "--listen" => listen = value(&mut it)?,
            "--port" => port = value(&mut it)?,
            "--token" => token = Some(value(&mut it)?),
            _ => return Err(format!("unknown option `{arg}`")),
        }
    }
    let token = token
        .filter(|t| t.len() >= control_auth::MIN_TOKEN_LEN)
        .ok_or_else(|| format!("web-control needs --token or GRIDCRAFT_CONTROL_TOKEN (at least {} characters)", control_auth::MIN_TOKEN_LEN))?;
    let ws_addr = gridcraft_mcp::control_addr(&listen);
    let tcp_addr = gridcraft_mcp::control_addr(&port);
    let ws = TcpListener::bind(&ws_addr).map_err(|e| format!("cannot listen for a browser at {ws_addr}: {e}"))?;
    let tcp = TcpListener::bind(&tcp_addr).map_err(|e| format!("cannot listen for MCP at {tcp_addr}: {e}"))?;
    eprintln!("gridcraft-cli: browser WebSocket at ws://{ws_addr}; MCP: gridcraft-cli mcp --connect {port}");
    serve(ws, tcp, token)
}

/// Runs on a worker with already-bound listeners; desktop uses private ephemeral ports.
pub fn serve(ws: TcpListener, tcp: TcpListener, token: String) -> Result<(), String> {
    for stream in ws.incoming() {
        match stream {
            Ok(stream) => {
                if let Err(e) = serve_browser(stream, &tcp, &token) {
                    eprintln!("gridcraft-cli: browser control disconnected: {e}");
                }
            }
            Err(e) => eprintln!("gridcraft-cli: browser connection failed: {e}"),
        }
    }
    Ok(())
}

fn serve_browser(stream: TcpStream, tcp: &TcpListener, token: &str) -> Result<(), String> {
    let local = stream.peer_addr().is_ok_and(|address| address.ip().is_loopback());
    stream.set_read_timeout(Some(std::time::Duration::from_secs(10))).map_err(|e| e.to_string())?;
    let mut ws = tungstenite::accept(stream).map_err(|e| e.to_string())?;
    authenticate_ws(&mut ws, token)?;
    ws.get_ref().set_read_timeout(Some(std::time::Duration::from_millis(100))).map_err(|e| e.to_string())?;
    tcp.set_nonblocking(true).map_err(|e| e.to_string())?;
    eprintln!("gridcraft-cli: browser control connected");
    loop {
        match tcp.accept() {
            Ok((mut stream, _)) => {
                stream.set_nonblocking(false).map_err(|e| e.to_string())?;
                ws.get_ref().set_read_timeout(None).map_err(|e| e.to_string())?;
                let result = serve_mcp(&mut stream, &mut ws, token);
                ws.get_ref().set_read_timeout(Some(std::time::Duration::from_millis(100))).map_err(|e| e.to_string())?;
                if let Err(e) = result {
                    eprintln!("gridcraft-cli: MCP control disconnected: {e}");
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => match ws.read() {
                Ok(Message::Ping(data)) => ws.send(Message::Pong(data)).map_err(|e| e.to_string())?,
                Ok(Message::Close(_)) => return Err("browser closed the control channel".into()),
                Ok(Message::Text(text)) => {
                    let request: Value = serde_json::from_str(&text).map_err(|e| format!("invalid browser request: {e}"))?;
                    ws.get_ref().set_read_timeout(None).map_err(|e| e.to_string())?;
                    let handled = if local && request.get("method").and_then(Value::as_str).is_some_and(|m| m.starts_with("server.")) {
                        let result = crate::remote::handle_local(request["method"].as_str().unwrap_or(""), &request["params"]);
                        let response = match result {
                            Ok(result) => json!({"id":request.get("id"),"result":result}),
                            Err(error) => json!({"id":request.get("id"),"error":{"message":error}}),
                        };
                        ws.send(Message::text(response.to_string())).map(|_| true).map_err(|e| e.to_string())
                    } else {
                        crate::web_agent::handle(&mut ws, &request)
                    };
                    ws.get_ref().set_read_timeout(Some(std::time::Duration::from_millis(100))).map_err(|e| e.to_string())?;
                    handled?;
                }
                Ok(_) => {}
                Err(tungstenite::Error::Io(e)) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => {}
                Err(e) => return Err(e.to_string()),
            },
            Err(e) => return Err(e.to_string()),
        }
        std::thread::sleep(std::time::Duration::from_millis(25));
    }
}

pub(crate) fn authenticate_ws<S: std::io::Read + Write>(ws: &mut WebSocket<S>, token: &str) -> Result<(), String> {
    let reply = match ws.read() {
        Ok(Message::Text(line)) => control_auth::check_first_line(&line, token),
        Ok(_) => Err(json!({"ok": false, "error": "authentication required"})),
        Err(e) => return Err(e.to_string()),
    };
    let allowed = reply.is_ok();
    let reply = reply.unwrap_or_else(|e| e);
    ws.send(Message::text(reply.to_string())).map_err(|e| e.to_string())?;
    if allowed { Ok(()) } else { Err("browser authentication failed".into()) }
}

fn serve_mcp(stream: &mut TcpStream, ws: &mut WebSocket<TcpStream>, token: &str) -> Result<(), String> {
    let mut reader = BufReader::new(stream.try_clone().map_err(|e| e.to_string())?);
    stream.set_read_timeout(Some(std::time::Duration::from_secs(10))).map_err(|e| e.to_string())?;
    if !authenticate_tcp(&mut reader, stream, token)? {
        return Ok(());
    }
    stream.set_read_timeout(None).map_err(|e| e.to_string())?;
    loop {
        let mut line = String::new();
        let n = std::io::Read::take(&mut reader, MAX_LINE).read_line(&mut line).map_err(|e| e.to_string())?;
        if n == 0 {
            return Ok(());
        }
        if !line.ends_with('\n') {
            return Err("control request is too large".into());
        }
        ws.send(Message::text(line.trim_end())).map_err(|e| e.to_string())?;
        loop {
            match ws.read() {
                Ok(Message::Text(reply)) => {
                    stream.write_all(reply.as_bytes()).map_err(|e| e.to_string())?;
                    stream.write_all(b"\n").map_err(|e| e.to_string())?;
                    stream.flush().map_err(|e| e.to_string())?;
                    break;
                }
                Ok(Message::Ping(data)) => ws.send(Message::Pong(data)).map_err(|e| e.to_string())?,
                Ok(Message::Close(_)) => return Err("browser closed the control channel".into()),
                Ok(_) => {}
                Err(e) => return Err(e.to_string()),
            }
        }
    }
}

fn authenticate_tcp(reader: &mut BufReader<TcpStream>, out: &mut TcpStream, token: &str) -> Result<bool, String> {
    let mut line = String::new();
    std::io::Read::take(reader, control_auth::MAX_AUTH_LINE as u64).read_line(&mut line).map_err(|e| e.to_string())?;
    let reply = control_auth::check_first_line(&line, token);
    let allowed = reply.is_ok();
    let reply: Value = reply.unwrap_or_else(|e| e);
    writeln!(out, "{reply}").map_err(|e| e.to_string())?;
    Ok(allowed)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    fn tcp_pair() -> (TcpStream, TcpStream) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let client = TcpStream::connect(address).unwrap();
        let (server, _) = listener.accept().unwrap();
        (client, server)
    }

    #[test]
    fn mcp_requires_the_shared_token() {
        let (mut client, mut server) = tcp_pair();
        writeln!(client, "{}", control_auth::auth_line(1, "wrong-token")).unwrap();
        let mut reader = BufReader::new(server.try_clone().unwrap());
        assert!(!authenticate_tcp(&mut reader, &mut server, "correct-token").unwrap());
        let mut response = String::new();
        BufReader::new(client).read_line(&mut response).unwrap();
        assert_eq!(serde_json::from_str::<Value>(&response).unwrap(), json!({"ok": false, "error": "authentication required"}));
    }

    #[test]
    fn mcp_accepts_the_shared_token() {
        let (mut client, mut server) = tcp_pair();
        writeln!(client, "{}", control_auth::auth_line(7, "correct-token")).unwrap();
        let mut reader = BufReader::new(server.try_clone().unwrap());
        assert!(authenticate_tcp(&mut reader, &mut server, "correct-token").unwrap());
        let mut response = String::new();
        BufReader::new(client).read_line(&mut response).unwrap();
        assert_eq!(serde_json::from_str::<Value>(&response).unwrap(), json!({"id": 7, "ok": true, "result": {"authenticated": true}}));
    }
}
