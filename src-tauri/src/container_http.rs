// SPDX-License-Identifier: AGPL-3.0-or-later
//! HTTP probes from the validator container, independent of published host ports.

const HTTP_SCRIPT: &str = include_str!("../../scripts/container-http.sh");

pub(crate) async fn request(
    host: &str,
    port: u16,
    method: &str,
    path: &str,
    body: &str,
) -> Result<String, String> {
    let context = format!("{method} {host}:{port}{path} via quip-validator");
    let mut command = crate::cmd::new("docker");
    command.args([
        "exec",
        "quip-validator",
        "timeout",
        "5",
        "bash",
        "-c",
        HTTP_SCRIPT,
        "--",
        host,
        &port.to_string(),
        method,
        path,
        body,
    ]);
    let mut command = tokio::process::Command::from(command);
    command.kill_on_drop(true);
    let output = tokio::time::timeout(std::time::Duration::from_secs(7), command.output())
        .await
        .map_err(|_| format!("{context}: timed out; check Docker and the service logs"))?
        .map_err(|e| format!("{context}: could not run Docker: {e}"))?;
    if !output.status.success() {
        return Err(format!(
            "{context}: {}; {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    parse_response(&output.stdout).map_err(|e| format!("{context}: {e}"))
}

fn parse_response(response: &[u8]) -> Result<String, String> {
    let response = std::str::from_utf8(response).map_err(|e| format!("invalid HTTP text: {e}"))?;
    let (headers, body) = response
        .split_once("\r\n\r\n")
        .ok_or("incomplete HTTP response")?;
    let status = headers.lines().next().ok_or("missing HTTP status")?;
    let code = status
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse::<u16>().ok())
        .ok_or("invalid HTTP status")?;
    if !(200..300).contains(&code) {
        return Err(format!("{status}: {body}"));
    }
    for line in headers.lines().skip(1) {
        if let Some((name, value)) = line.split_once(':') {
            if name.eq_ignore_ascii_case("content-length") {
                let expected = value.trim().parse::<usize>().map_err(|e| e.to_string())?;
                if body.len() != expected {
                    return Err("truncated HTTP response body".into());
                }
            }
            if name.eq_ignore_ascii_case("transfer-encoding") {
                return Err("unexpected transfer encoding in HTTP/1.0 response".into());
            }
        }
    }
    Ok(body.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn http_failures_and_truncated_bodies_are_reported() {
        assert_eq!(
            parse_response(b"HTTP/1.0 200 OK\r\nContent-Length: 2\r\n\r\n{}").unwrap(),
            "{}"
        );
        assert!(parse_response(b"HTTP/1.0 403 Forbidden\r\n\r\ndenied")
            .unwrap_err()
            .contains("403"));
        assert!(parse_response(b"HTTP/1.0 200 OK\r\nContent-Length: 9\r\n\r\n{}").is_err());
        assert!(parse_response(b"HTTP/1.0 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n0").is_err());
        assert!(parse_response(b"not HTTP").is_err());
    }

    #[cfg(unix)]
    #[test]
    fn probe_script_sends_a_request_and_reads_the_actual_http_response() {
        check_probe_request("GET", "/api/v1/status", "");
        check_probe_request(
            "POST",
            "/",
            r#"{"jsonrpc":"2.0","method":"system_health","params":["λ;$(false)"],"id":1}"#,
        );
    }

    #[cfg(unix)]
    fn check_probe_request(method: &'static str, path: &'static str, body: &'static str) {
        use std::io::{Read, Write};
        use std::net::TcpListener;
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(std::time::Duration::from_secs(3)))
                .unwrap();
            let mut request = Vec::new();
            let mut byte = [0];
            while !request.ends_with(b"\r\n\r\n") {
                stream.read_exact(&mut byte).unwrap();
                request.push(byte[0]);
            }
            let headers = String::from_utf8(request).unwrap();
            assert!(headers.starts_with(&format!("{method} {path} HTTP/1.0")));
            assert!(headers.contains(&format!("Content-Length: {}\r\n", body.len())));
            let mut received_body = vec![0; body.len()];
            stream.read_exact(&mut received_body).unwrap();
            assert_eq!(received_body, body.as_bytes());
            stream
                .write_all(b"HTTP/1.0 200 OK\r\nContent-Length: 11\r\n\r\n{\"ok\":true}")
                .unwrap();
        });
        let output = std::process::Command::new("bash")
            .args([
                "-c",
                HTTP_SCRIPT,
                "--",
                "127.0.0.1",
                &port.to_string(),
                method,
                path,
                body,
            ])
            .output()
            .unwrap();
        server.join().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(parse_response(&output.stdout).unwrap(), r#"{"ok":true}"#);
    }
}
