//! End-to-end tests for the virtual overlay network.
//!
//! Three scenarios live here:
//!
//! * [`unprivileged_overlay_round_trip`] — runs in plain `cargo test` with NO
//!   root. It exercises the overlay components that are reachable through the
//!   daemon's public API: VIP allocation, the [`ServiceTable`], the embedded
//!   authoritative DNS server (`name.portzero.local` -> VIP), and a real tokio
//!   backend that the overlay would proxy to. This asserts the *wiring*
//!   (name -> VIP -> real backend).
//!
//! * [`unprivileged_vip_byte_proxy`] — also unprivileged. It drives REAL bytes
//!   through the smoltcp user-space `StackEngine`: a client smoltcp interface
//!   connects to `VIP:port` across an in-memory `MockDevice`, and the payload is
//!   proxied to a real tokio backend and echoed back. This asserts the actual
//!   byte path (mirroring the in-crate unit test
//!   `net::stack::tests::vip_connect_proxies_to_backend`) using the
//!   `#[doc(hidden)]` test-support helpers exposed from `net::stack`.
//!
//! * [`real_tun_overlay`] — gated behind `PORTZERO_REQUIRE_REAL_TUN_E2E=1` on
//!   every platform, then behind the platform privilege check (root on Unix,
//!   elevated Administrator on Windows). Plain `cargo test` skips cleanly; run
//!   it via `just e2e` when you want the real TUN/Wintun path. This verifies
//!   both DNS and a real OS TCP socket talking to a VIP through the tunnel.
//!
//! Every wait is bounded by a timeout — there is no unbounded blocking.

use std::net::{Ipv4Addr, SocketAddr};
use std::time::Duration;

use portzero_daemon::net::dns::OverlayDnsServer;
use portzero_daemon::net::service_table::ServiceTable;

use hickory_proto::op::{Message, MessageType, OpCode};
use hickory_proto::rr::{DNSClass, Name, RData, RecordType};
use hickory_proto::serialize::binary::{BinDecodable, BinEncodable};

use std::str::FromStr;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, UdpSocket};
use tokio::sync::RwLock;

/// Overall safety net so a stuck test never hangs CI.
const TEST_TIMEOUT: Duration = Duration::from_secs(15);

/// Spawn a tiny TCP echo server bound to an ephemeral port (the "port 0"
/// backend a real service would expose). Returns its real address.
async fn spawn_echo_backend() -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let (mut sock, _) = match listener.accept().await {
                Ok(p) => p,
                Err(_) => break,
            };
            tokio::spawn(async move {
                let mut buf = vec![0u8; 4096];
                loop {
                    match sock.read(&mut buf).await {
                        Ok(0) => break,
                        Ok(n) => {
                            if sock.write_all(&buf[..n]).await.is_err() {
                                break;
                            }
                        }
                        Err(_) => break,
                    }
                }
            });
        }
    });
    addr
}

/// Send a single A query for `name` to the DNS server at `dns_addr` and return
/// the first A record's address, if any. Bounded by `TEST_TIMEOUT`.
async fn dns_query_a(dns_addr: SocketAddr, name: &str) -> Option<Ipv4Addr> {
    let sock = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    sock.connect(dns_addr).await.unwrap();

    let mut req = Message::new();
    req.set_id(0x1234);
    req.set_message_type(MessageType::Query);
    req.set_op_code(OpCode::Query);
    req.set_recursion_desired(true);
    let mut q = hickory_proto::op::Query::new();
    q.set_name(Name::from_str(name).unwrap());
    q.set_query_type(RecordType::A);
    q.set_query_class(DNSClass::IN);
    req.add_query(q);

    let bytes = req.to_bytes().unwrap();
    sock.send(&bytes).await.unwrap();

    let mut buf = vec![0u8; 512];
    let n = tokio::time::timeout(TEST_TIMEOUT, sock.recv(&mut buf))
        .await
        .expect("DNS query timed out")
        .expect("DNS recv failed");

    let resp = Message::from_bytes(&buf[..n]).unwrap();
    for ans in resp.answers() {
        if let Some(RData::A(a)) = ans.data() {
            return Some(a.0);
        }
    }
    None
}

/// Connect to `addr`, send `payload`, and require the exact echo. Retries are
/// intentionally short-lived: the privileged e2e may race listener installation
/// or route propagation for a few milliseconds after `update_services`.
async fn assert_tcp_echo(addr: SocketAddr, payload: &[u8]) {
    let mut last_error = String::new();

    for _ in 0..20 {
        let attempt = tokio::time::timeout(Duration::from_millis(250), async {
            let mut client = tokio::net::TcpStream::connect(addr).await?;
            client.write_all(payload).await?;
            let mut got = vec![0u8; payload.len()];
            client.read_exact(&mut got).await?;
            Ok::<Vec<u8>, std::io::Error>(got)
        })
        .await;

        match attempt {
            Ok(Ok(got)) if got == payload => return,
            Ok(Ok(got)) => {
                last_error = format!("echo mismatch: got {got:?}, expected {payload:?}");
            }
            Ok(Err(e)) => {
                last_error = e.to_string();
            }
            Err(_) => {
                last_error = "connect/read/write timed out".to_string();
            }
        }

        tokio::time::sleep(Duration::from_millis(25)).await;
    }

    #[cfg(target_os = "windows")]
    let diagnostics = windows_route_diagnostics(addr);
    #[cfg(not(target_os = "windows"))]
    let diagnostics = String::new();

    panic!("TCP tunnel echo to {addr} failed: {last_error}{diagnostics}");
}

#[cfg(target_os = "windows")]
fn windows_route_diagnostics(addr: SocketAddr) -> String {
    let ip = addr.ip();
    let script = format!(
        r#"
$ErrorActionPreference = "Continue"
$ip = "{ip}"
$alias = "portzero-e2e"
"`n--- portzero-e2e adapter ---"
Get-NetAdapter -Name $alias -ErrorAction SilentlyContinue | Format-List Name,InterfaceIndex,Status,MacAddress,LinkSpeed
"`n--- portzero-e2e addresses ---"
Get-NetIPAddress -InterfaceAlias $alias -AddressFamily IPv4 -ErrorAction SilentlyContinue | Format-List IPAddress,PrefixLength,PrefixOrigin,SuffixOrigin,AddressState
"`n--- route selected for VIP ---"
Find-NetRoute -RemoteIPAddress $ip -ErrorAction SilentlyContinue | Format-List DestinationPrefix,NextHop,InterfaceAlias,InterfaceIndex,RouteMetric,InterfaceMetric,PolicyStore
"`n--- relevant routes ---"
Get-NetRoute -AddressFamily IPv4 -ErrorAction SilentlyContinue |
  Where-Object {{ $_.DestinationPrefix -eq "$ip/32" -or $_.DestinationPrefix -eq "10.254.0.0/16" -or $_.InterfaceAlias -eq $alias }} |
  Sort-Object DestinationPrefix,RouteMetric |
  Format-Table DestinationPrefix,NextHop,InterfaceAlias,InterfaceIndex,RouteMetric,InterfaceMetric,PolicyStore -AutoSize
"`n--- netsh ipv4 config ---"
netsh interface ipv4 show config name="$alias"
"#
    );

    match std::process::Command::new("powershell.exe")
        .args([
            "-NoProfile",
            "-ExecutionPolicy",
            "Bypass",
            "-Command",
            &script,
        ])
        .output()
    {
        Ok(output) => format!(
            "\n{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        ),
        Err(e) => format!("\nfailed to collect Windows route diagnostics: {e}"),
    }
}

#[cfg(target_os = "windows")]
struct WindowsHostRoute {
    prefix: String,
    interface_alias: &'static str,
}

#[cfg(target_os = "windows")]
impl WindowsHostRoute {
    fn install(ip: Ipv4Addr, interface_alias: &'static str) -> Self {
        let prefix = format!("{ip}/32");
        let script = format!(
            r#"
$ErrorActionPreference = "Stop"
$prefix = "{prefix}"
$alias = "{interface_alias}"
Remove-NetRoute -DestinationPrefix $prefix -InterfaceAlias $alias -Confirm:$false -ErrorAction SilentlyContinue
$adapter = Get-NetAdapter -Name $alias -ErrorAction Stop
New-NetRoute -DestinationPrefix $prefix -InterfaceIndex $adapter.InterfaceIndex -NextHop 0.0.0.0 -RouteMetric 1 -PolicyStore ActiveStore | Out-Null
"#
        );
        let output = std::process::Command::new("powershell.exe")
            .args([
                "-NoProfile",
                "-ExecutionPolicy",
                "Bypass",
                "-Command",
                &script,
            ])
            .output()
            .expect("failed to run PowerShell to install Windows e2e host route");

        assert!(
            output.status.success(),
            "failed to install Windows e2e host route {prefix} on {interface_alias}: {}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );

        Self {
            prefix,
            interface_alias,
        }
    }
}

#[cfg(target_os = "windows")]
impl Drop for WindowsHostRoute {
    fn drop(&mut self) {
        let script = format!(
            r#"
Remove-NetRoute -DestinationPrefix "{}" -InterfaceAlias "{}" -Confirm:$false -ErrorAction SilentlyContinue
"#,
            self.prefix, self.interface_alias
        );
        let _ = std::process::Command::new("powershell.exe")
            .args([
                "-NoProfile",
                "-ExecutionPolicy",
                "Bypass",
                "-Command",
                &script,
            ])
            .status();
    }
}

#[cfg(target_os = "macos")]
struct MacHostRoute {
    ip: String,
    interface_name: String,
}

#[cfg(target_os = "macos")]
impl MacHostRoute {
    fn install(ip: Ipv4Addr, interface_name: &str) -> Self {
        let ip = ip.to_string();
        let output = std::process::Command::new("route")
            .args(["-n", "add", "-host", &ip, "-interface", interface_name])
            .output()
            .expect("failed to run route to install macOS e2e host route");

        assert!(
            output.status.success(),
            "failed to install macOS e2e host route {ip}/32 on {interface_name}: {}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );

        Self {
            ip,
            interface_name: interface_name.to_string(),
        }
    }
}

#[cfg(target_os = "macos")]
impl Drop for MacHostRoute {
    fn drop(&mut self) {
        let _ = std::process::Command::new("route")
            .args([
                "-n",
                "delete",
                "-host",
                &self.ip,
                "-interface",
                &self.interface_name,
            ])
            .status();
    }
}

#[cfg(target_os = "linux")]
struct LinuxHostRoute {
    prefix: String,
    iface: &'static str,
}

#[cfg(target_os = "linux")]
impl LinuxHostRoute {
    fn install(ip: Ipv4Addr, iface: &'static str) -> Self {
        let prefix = format!("{ip}/32");
        let output = std::process::Command::new("ip")
            .args(["route", "replace", &prefix, "dev", iface])
            .output()
            .expect("failed to run ip to install Linux e2e host route");

        assert!(
            output.status.success(),
            "failed to install Linux e2e host route {prefix} on {iface}: {}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );

        Self { prefix, iface }
    }
}

#[cfg(target_os = "linux")]
impl Drop for LinuxHostRoute {
    fn drop(&mut self) {
        let _ = std::process::Command::new("ip")
            .args(["route", "del", &self.prefix, "dev", self.iface])
            .status();
    }
}

/// Unprivileged, in-process e2e: wires together the public overlay surface and
/// asserts name -> VIP resolution plus a reachable real backend. No root, no
/// real TUN, no network/cloud login. All waits are bounded.
#[tokio::test]
async fn unprivileged_overlay_round_trip() {
    tokio::time::timeout(TEST_TIMEOUT, async {
        // 1. A real backend bound to an ephemeral ("port 0") address.
        let backend_addr = spawn_echo_backend().await;

        // 2. Register a `*.portzero.local` service in the shared table the DNS
        //    server reads from. The service is reachable on VIP:5432 and proxies
        //    to the real ephemeral backend.
        let services: Arc<RwLock<ServiceTable>> = Arc::new(RwLock::new(ServiceTable::new()));
        let vip = {
            let mut table = services.write().await;
            let svc = table.register("my-db".to_string(), backend_addr, 5432, 0);
            // VIP must come from the 10.254.0.0/16 overlay block.
            let o = svc.vip.0;
            assert_eq!([o[0], o[1]], [10, 254], "VIP not in overlay subnet");
            Ipv4Addr::new(o[0], o[1], o[2], o[3])
        };

        // Re-registering the same name must keep the same VIP (stable identity).
        {
            let mut table = services.write().await;
            let svc = table.register("my-db".to_string(), backend_addr, 5432, 0);
            assert_eq!(svc.vip.0, vip.octets(), "VIP changed on re-register");
        }

        // 3. Stand up the *real* embedded authoritative DNS server on an
        //    ephemeral UDP port and confirm name -> VIP resolution end to end.
        let dns_addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
        // Bind ourselves to learn the chosen port, then hand the bound addr to
        // the server (it rebinds; both use SO_REUSEADDR-free fresh sockets, so
        // we instead let the server bind 0 and discover via a probe loop).
        let probe = UdpSocket::bind(dns_addr).await.unwrap();
        let server_addr = probe.local_addr().unwrap();
        drop(probe); // free the port for the server to claim immediately

        let dns = OverlayDnsServer::new(services.clone(), server_addr);
        let dns_task = tokio::spawn(async move {
            let _ = dns.run().await;
        });

        // Resolve the registered name; retry briefly while the server binds.
        let mut resolved = None;
        for _ in 0..50 {
            if let Some(ip) = dns_query_a(server_addr, "my-db.portzero.local").await {
                resolved = Some(ip);
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert_eq!(
            resolved,
            Some(vip),
            "DNS did not resolve my-db.portzero.local to its VIP"
        );

        // An unknown name under our zone must NOT resolve to an address.
        let unknown = dns_query_a(server_addr, "nope.portzero.local").await;
        assert_eq!(unknown, None, "unknown name unexpectedly resolved");

        // 4. The real backend the overlay proxies to is reachable and echoes.
        //    (The smoltcp byte-proxy across the VIP is asserted separately by
        //    `unprivileged_vip_byte_proxy`.)
        let mut client = tokio::net::TcpStream::connect(backend_addr).await.unwrap();
        let payload = b"hello virtual overlay";
        client.write_all(payload).await.unwrap();
        let mut got = vec![0u8; payload.len()];
        client.read_exact(&mut got).await.unwrap();
        assert_eq!(&got, payload, "backend did not echo payload");

        dns_task.abort();
    })
    .await
    .expect("unprivileged overlay round-trip timed out");
}

/// Unprivileged, in-process e2e that drives REAL bytes through the smoltcp
/// `StackEngine`. Unlike [`unprivileged_overlay_round_trip`] (which asserts the
/// public wiring + a directly-reachable backend), this test connects a client
/// smoltcp interface to `VIP:port` across an in-memory [`MockDevice`], so the
/// payload actually traverses the user-space TCP stack and is proxied to the
/// real tokio backend and echoed back. No root, no real TUN. All waits bounded.
#[tokio::test]
async fn unprivileged_vip_byte_proxy() {
    use portzero_daemon::net::stack::{
        client_iface, new_tcp_socket, test_tcp, MockDevice, OverlayHttpsPolicy, TestInstant,
        TestIpAddress, TestIpv4Address, TestSocketSet, VirtualStack,
    };

    tokio::time::timeout(TEST_TIMEOUT, async {
        // 1. Start a real backend echo server (the "port 0" backend).
        let backend_addr = spawn_echo_backend().await;

        // 2. Register a `*.portzero.local` service mapping VIP:5432 -> backend.
        let mut table = ServiceTable::new();
        let svc = table.register("my-db".to_string(), backend_addr, 5432, 0);
        let vip = svc.vip;
        let vip_v4 = TestIpv4Address::from_bytes(&vip.0);

        // 3. Spawn the real stack engine on one half of an in-memory device pair.
        let (stack_dev, mut client_dev) = MockDevice::pair();
        let stack =
            VirtualStack::spawn_with_device(stack_dev, table, None, OverlayHttpsPolicy::default());

        // 4. Build a client smoltcp interface on the other half and open a TCP
        //    connection to VIP:5432 from a client IP in the same subnet.
        let client_ip = TestIpv4Address::new(10, 254, 9, 9);
        let mut client_iface = client_iface(&mut client_dev, client_ip);
        let mut client_sockets = TestSocketSet::new(Vec::new());
        let client_handle = client_sockets.add(new_tcp_socket());
        {
            let sock = client_sockets.get_mut::<test_tcp::Socket>(client_handle);
            let cx = client_iface.context();
            sock.connect(
                cx,
                (TestIpAddress::Ipv4(vip_v4), 5432u16),
                (client_ip, 49000u16),
            )
            .unwrap();
        }

        // 5. Drive both sides: send a payload through the stack and assert the
        //    backend echo comes back across the user-space byte-proxy.
        let payload = b"hello virtual overlay";
        let mut sent = false;
        let mut received = Vec::new();

        for _ in 0..2000 {
            client_iface.poll(TestInstant::now(), &mut client_dev, &mut client_sockets);

            let sock = client_sockets.get_mut::<test_tcp::Socket>(client_handle);
            if sock.may_send() && sock.can_send() && !sent {
                sock.send_slice(payload).unwrap();
                sent = true;
            }
            if sock.can_recv() {
                let mut buf = vec![0u8; 4096];
                if let Ok(n) = sock.recv_slice(&mut buf) {
                    received.extend_from_slice(&buf[..n]);
                }
            }
            if received.len() >= payload.len() {
                break;
            }

            // Let the stack task run its poll loop and the backend echo.
            tokio::time::sleep(Duration::from_millis(2)).await;
        }

        assert!(
            sent,
            "client never reached a sendable state (handshake failed)"
        );
        assert_eq!(
            received, payload,
            "byte-proxy did not echo payload through the smoltcp stack"
        );

        stack.shutdown().await.unwrap();
    })
    .await
    .expect("unprivileged vip byte-proxy timed out");
}

/// Unprivileged TLS e2e: a smoltcp client connects to VIP:443 through the real
/// `StackEngine` (in-memory [`MockDevice`]) and completes a full TLS handshake
/// backed by our locally-generated wildcard cert. The TLS payload is forwarded
/// in plaintext to a real tokio echo backend; the echo comes back encrypted and
/// the client decrypts it via the same [`rustls::ClientConnection`].
///
/// No root, no TUN, no disk I/O — cert material is generated in-memory via
/// [`LocalCa::generate_ephemeral`]. Platform-neutral: runs on Linux, macOS,
/// and Windows without any `cfg` guards.
#[tokio::test]
async fn unprivileged_tls_vip_proxy() {
    use std::io::Read as _;
    use std::io::Write as _;

    use portzero_daemon::net::stack::{
        client_iface, new_tcp_socket, test_tcp, MockDevice, OverlayHttpsPolicy, TestInstant,
        TestIpAddress, TestIpv4Address, TestSocketSet, VirtualStack,
    };
    use portzero_daemon::tls::{ca::LocalCa, stack::build_server_config};
    use rustls::pki_types::ServerName;

    // Both ring and aws-lc-rs are compiled in (via reqwest/hyper-rustls), so
    // rustls cannot auto-select a provider — install ring explicitly.
    // .ok() silently ignores "already installed" when tests share a process.
    let _ = rustls::crypto::ring::default_provider().install_default();

    tokio::time::timeout(TEST_TIMEOUT, async {
        // 1. Generate a fresh CA + wildcard cert entirely in memory (no disk I/O).
        let ca = LocalCa::generate_ephemeral().unwrap();
        let server_config = build_server_config(&ca).unwrap();

        // 2. Build a rustls ClientConfig that trusts our in-memory CA.
        let mut root_store = rustls::RootCertStore::empty();
        let ca_cert_der = rustls_pemfile::certs(&mut ca.ca_cert_pem.as_bytes())
            .next()
            .unwrap()
            .unwrap();
        root_store.add(ca_cert_der).unwrap();
        let client_config = Arc::new(
            rustls::ClientConfig::builder()
                .with_root_certificates(root_store)
                .with_no_client_auth(),
        );
        let server_name = ServerName::try_from("hello.portzero.local")
            .unwrap()
            .to_owned();
        let mut tls_conn = rustls::ClientConnection::new(client_config, server_name).unwrap();

        // 3. Plain TCP echo backend — receives cleartext bytes from the TLS
        //    terminator, so it never sees ciphertext.
        let backend_addr = spawn_echo_backend().await;

        // 4. Register the service on its HTTP port (80). The stack accepts
        //    VIP:443, terminates TLS, and forwards plaintext to this backend
        //    via VIP lookup (not port matching).
        let mut table = ServiceTable::new();
        let svc = table.register("hello".to_string(), backend_addr, 80, 0);
        let vip = svc.vip;
        let vip_v4 = TestIpv4Address::from_bytes(&vip.0);

        // 5. Spawn the stack engine with TLS enabled.
        let (stack_dev, mut client_dev) = MockDevice::pair();
        let stack = VirtualStack::spawn_with_device(
            stack_dev,
            table,
            Some(server_config),
            OverlayHttpsPolicy::default(),
        );

        // 6. Build a smoltcp client interface and open a TCP connection to VIP:443.
        let client_ip = TestIpv4Address::new(10, 254, 9, 9);
        let mut client_iface = client_iface(&mut client_dev, client_ip);
        let mut client_sockets = TestSocketSet::new(Vec::new());
        let client_handle = client_sockets.add(new_tcp_socket());
        {
            let sock = client_sockets.get_mut::<test_tcp::Socket>(client_handle);
            let cx = client_iface.context();
            sock.connect(
                cx,
                (TestIpAddress::Ipv4(vip_v4), 443u16),
                (client_ip, 49000u16),
            )
            .unwrap();
        }

        // 7. Drive the smoltcp poll loop interleaved with the rustls
        //    ClientConnection state machine.
        //
        //    Each iteration:
        //      a) poll smoltcp to move packets between client and stack;
        //      b) feed received ciphertext bytes into rustls;
        //      c) drain any decrypted application data;
        //      d) queue our payload once the handshake is done;
        //      e) write any pending TLS output into the smoltcp send buffer.
        let payload = b"hello tls overlay";
        let mut plaintext_sent = false;
        let mut received = Vec::new();

        for _ in 0..3000 {
            client_iface.poll(TestInstant::now(), &mut client_dev, &mut client_sockets);

            let sock = client_sockets.get_mut::<test_tcp::Socket>(client_handle);

            // (b) Feed incoming TLS ciphertext to rustls.
            if sock.can_recv() {
                let mut buf = vec![0u8; 4096];
                if let Ok(n) = sock.recv_slice(&mut buf) {
                    if n > 0 {
                        buf.truncate(n);
                        tls_conn.read_tls(&mut buf.as_slice()).unwrap();
                        tls_conn
                            .process_new_packets()
                            .expect("TLS processing error");
                    }
                }
            }

            // (c) Drain any decrypted application data (the backend echo).
            {
                let mut tmp = [0u8; 4096];
                loop {
                    match tls_conn.reader().read(&mut tmp) {
                        Ok(0) => break,
                        Ok(n) => received.extend_from_slice(&tmp[..n]),
                        Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                        Err(_) => break,
                    }
                }
            }

            // (d) Once the handshake completes, queue our payload for encryption.
            if !tls_conn.is_handshaking() && !plaintext_sent {
                tls_conn.writer().write_all(payload).unwrap();
                plaintext_sent = true;
            }

            // (e) Write any pending TLS output bytes into the smoltcp socket.
            if sock.may_send() && sock.can_send() {
                let mut tls_out = Vec::new();
                let _ = tls_conn.write_tls(&mut tls_out);
                if !tls_out.is_empty() {
                    let _ = sock.send_slice(&tls_out);
                }
            }

            if received.len() >= payload.len() {
                break;
            }

            tokio::time::sleep(Duration::from_millis(2)).await;
        }

        assert!(
            plaintext_sent,
            "TLS handshake never completed (plaintext was never queued)"
        );
        assert_eq!(
            &received[..payload.len().min(received.len())],
            payload,
            "TLS-proxied echo did not match payload"
        );

        stack.shutdown().await.unwrap();
    })
    .await
    .expect("unprivileged TLS vip proxy timed out");
}

/// Privileged real-TUN e2e. Skips unless explicitly opted in, so ordinary
/// `cargo test` stays side-effect safe even if it is run as root/Admin.
///
/// This is the path behind `just e2e`: it creates the real OS TUN/Wintun
/// interface, registers a service, resolves its overlay name via the embedded
/// DNS server, then opens a normal OS TCP socket to the VIP and verifies that
/// bytes pass through the tunnel to the backend and back.
#[cfg(any(unix, target_os = "windows"))]
#[tokio::test]
async fn real_tun_overlay() {
    if std::env::var("PORTZERO_REQUIRE_REAL_TUN_E2E").as_deref() != Ok("1") {
        println!(
            "real_tun_overlay: skipped: set PORTZERO_REQUIRE_REAL_TUN_E2E=1 to run privileged real-TUN e2e"
        );
        return;
    }

    #[cfg(unix)]
    {
        // SAFETY: geteuid is always safe to call.
        let euid = unsafe { libc::geteuid() };
        if euid != 0 {
            println!("real_tun_overlay: skipped: requires root (euid={euid})");
            return;
        }
    }
    #[cfg(target_os = "windows")]
    {
        let elevated = std::process::Command::new("fltmc")
            .arg("filters")
            .status()
            .map(|status| status.success())
            .unwrap_or(false);
        assert!(
            elevated,
            "real_tun_overlay requires an elevated Administrator process on Windows"
        );
    }

    use portzero_daemon::net::overlay::{OverlayConfig, OverlayNetwork};
    use portzero_daemon::net::tun_device::TunConfig;

    println!("real_tun_overlay: bringing up real overlay");

    let result = tokio::time::timeout(TEST_TIMEOUT, async {
        // Use the embedded DNS on a high local port to avoid clashing with the
        // system resolver during the test.
        #[cfg(any(target_os = "linux", target_os = "windows"))]
        let mut tun = TunConfig::default();

        #[cfg(not(any(target_os = "linux", target_os = "windows")))]
        let tun = TunConfig::default();

        #[cfg(target_os = "linux")]
        {
            // Do not collide with the real daemon's default TUN interface
            // (`deven0`). `just e2e` is commonly run while the daemon is
            // installed or active.
            tun.name = Some("portzero-e2e".to_string());
            // Keep the test gateway inside 10.254.0.0/16 without reusing the
            // daemon's production gateway address.
            tun.address = Ipv4Addr::new(10, 254, 250, 1);
        }

        #[cfg(target_os = "windows")]
        {
            // Do not collide with the real daemon's default Wintun adapter
            // (`deven0`). Wintun permits only one active session per adapter,
            // and `just e2e` is commonly run while the daemon is installed.
            tun.name = Some("portzero-e2e".to_string());
            // Windows rejects assigning the same static IPv4 address to two
            // adapters. Keep this test off the production gateway address while
            // staying inside 10.254.0.0/16 so VIP routes still target the real
            // overlay subnet.
            tun.address = Ipv4Addr::new(10, 254, 250, 1);
        }

        let config = OverlayConfig {
            dns_listen: "127.0.0.1:53000".parse().unwrap(),
            tun,
            ..Default::default()
        };

        let overlay =
            OverlayNetwork::start(config, std::sync::Arc::new(tokio::sync::Notify::new()))
                .await
                .expect("overlay start failed under root");

        // Register a service so the stack installs a listener and DNS answers.
        let backend_addr = spawn_echo_backend().await;
        let mut table = ServiceTable::new();
        table.register("rooted".to_string(), backend_addr, 5432, 0);
        overlay
            .update_services(table)
            .await
            .expect("update_services failed");

        // Give the stack a moment to apply listeners, then resolve via the real
        // embedded DNS.
        tokio::time::sleep(Duration::from_millis(200)).await;
        let ip = dns_query_a("127.0.0.1:53000".parse().unwrap(), "rooted.portzero.local").await;
        let ip = ip.expect("real overlay DNS did not resolve service");
        #[cfg(target_os = "linux")]
        let _route = LinuxHostRoute::install(ip, "portzero-e2e");
        #[cfg(target_os = "macos")]
        let _route = MacHostRoute::install(ip, overlay.link_name());
        #[cfg(target_os = "windows")]
        let _route = WindowsHostRoute::install(ip, "portzero-e2e");

        // Prove this is a real end-to-end tunnel check: a normal OS TCP socket
        // connects to the service VIP, the packet enters the TUN/Wintun
        // interface, the stack proxies it to the localhost backend, and the echo
        // comes back through the same path.
        let tunnel_addr = SocketAddr::from((ip, 5432));
        assert_tcp_echo(tunnel_addr, b"hello real tun overlay").await;

        overlay.shutdown().await;
    })
    .await;

    result.expect("real_tun_overlay timed out");
    println!("real_tun_overlay: passed");
}

/// Non-Unix builds still compile and report this privileged Unix TUN scenario
/// as skipped when they do not support the privileged test path.
#[cfg(not(any(unix, target_os = "windows")))]
#[tokio::test]
async fn real_tun_overlay() {
    println!("real_tun_overlay: skipped: unsupported platform");
}
