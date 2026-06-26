//! Verify that a given PID has a socket in LISTEN state on a given port.

/// Returns `true` if `pid` has a TCP socket in LISTEN state on `port`.
pub fn pid_is_listening_on(pid: u32, port: u16) -> bool {
    pid_is_listening_impl(pid, port)
}

// ─── Linux ───────────────────────────────────────────────────────────────────

#[cfg(target_os = "linux")]
fn pid_is_listening_impl(pid: u32, port: u16) -> bool {
    // Parse /proc/{pid}/net/tcp and /proc/{pid}/net/tcp6.
    // State column (index 3) value 0A (hex) = LISTEN.
    for suffix in &["tcp", "tcp6"] {
        let path = format!("/proc/{}/net/{}", pid, suffix);
        let content = match std::fs::read_to_string(&path) {
            Ok(c) => c,
            Err(_) => continue,
        };
        for line in content.lines().skip(1) {
            let fields: Vec<&str> = line.split_whitespace().collect();
            // local_address: column 1 (index 1)
            // state:         column 4 (index 3)
            if fields.len() < 4 {
                continue;
            }
            let state = fields[3];
            if !state.eq_ignore_ascii_case("0A") {
                continue;
            }
            let local_addr = fields[1];
            if let Some(colon) = local_addr.rfind(':') {
                let port_hex = &local_addr[colon + 1..];
                if let Ok(p) = u16::from_str_radix(port_hex, 16) {
                    if p == port {
                        return true;
                    }
                }
            }
        }
    }
    false
}

// ─── macOS ───────────────────────────────────────────────────────────────────

#[cfg(target_os = "macos")]
fn pid_is_listening_impl(pid: u32, port: u16) -> bool {
    use std::process::Command;

    let output = match Command::new("lsof")
        .args([
            "-nP",
            &format!("-iTCP:{}", port),
            "-sTCP:LISTEN",
            "-p",
            &pid.to_string(),
        ])
        .output()
    {
        Ok(o) => o,
        Err(e) => {
            tracing::warn!("port_verify: lsof failed: {e}");
            // Skip verification on error — fail open.
            return true;
        }
    };

    output.status.success() && !output.stdout.is_empty()
}

// ─── Windows ─────────────────────────────────────────────────────────────────

#[cfg(target_os = "windows")]
fn pid_is_listening_impl(pid: u32, port: u16) -> bool {
    use std::process::Command;

    let output = match Command::new("netstat").args(["-ano"]).output() {
        Ok(o) => o,
        Err(e) => {
            tracing::warn!("port_verify: netstat failed: {e}");
            return true;
        }
    };

    let stdout = String::from_utf8_lossy(&output.stdout);
    let port_suffix = format!(":{}", port);
    for line in stdout.lines() {
        let fields: Vec<&str> = line.split_whitespace().collect();
        // Protocol  LocalAddr  ForeignAddr  State  PID
        if fields.len() < 5 {
            continue;
        }
        if fields[0].eq_ignore_ascii_case("TCP")
            && fields[3].eq_ignore_ascii_case("LISTENING")
            && fields[1].ends_with(&port_suffix)
        {
            if let Ok(p) = fields[4].parse::<u32>() {
                if p == pid {
                    return true;
                }
            }
        }
    }
    false
}

// ─── Unsupported platforms ────────────────────────────────────────────────────

#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
fn pid_is_listening_impl(_pid: u32, _port: u16) -> bool {
    // Skip verification on unknown platforms — fail open.
    true
}
