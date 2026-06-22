//! End-to-end tests for the virtual overlay network.
//!
//! Three scenarios live here:
//!
//! * [`unprivileged_overlay_round_trip`] — runs in plain `cargo test` with NO
//!   root. It exercises the overlay components that are reachable through the
//!   daemon's public API: VIP allocation, the [`ServiceTable`], the embedded
//!   authoritative DNS server (`name.devenv.local` -> VIP), and a real tokio
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
//! * [`real_tun_overlay`] — gated on `geteuid() == 0`. When not root it SKIPS
//!   cleanly (prints a clear line and passes) so unprivileged `cargo test` never
//!   fails or hangs. When root it stands up a real [`OverlayNetwork`] (TUN +
//!   stack + DNS) and tears it down. Run it via `just e2e` (uses `sudo`).
//!
//! Every wait is bounded by a timeout — there is no unbounded blocking.

use std::net::{Ipv4Addr, SocketAddr};
use std::time::Duration;

use devenv_tunnel_daemon::net::dns::OverlayDnsServer;
use devenv_tunnel_daemon::net::service_table::ServiceTable;

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

/// Unprivileged, in-process e2e: wires together the public overlay surface and
/// asserts name -> VIP resolution plus a reachable real backend. No root, no
/// real TUN, no network/cloud login. All waits are bounded.
#[tokio::test]
async fn unprivileged_overlay_round_trip() {
    tokio::time::timeout(TEST_TIMEOUT, async {
        // 1. A real backend bound to an ephemeral ("port 0") address.
        let backend_addr = spawn_echo_backend().await;

        // 2. Register a `*.devenv.local` service in the shared table the DNS
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
            if let Some(ip) = dns_query_a(server_addr, "my-db.devenv.local").await {
                resolved = Some(ip);
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert_eq!(
            resolved,
            Some(vip),
            "DNS did not resolve my-db.devenv.local to its VIP"
        );

        // An unknown name under our zone must NOT resolve to an address.
        let unknown = dns_query_a(server_addr, "nope.devenv.local").await;
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
    use devenv_tunnel_daemon::net::stack::{
        client_iface, new_tcp_socket, test_tcp, MockDevice, TestInstant, TestIpAddress,
        TestIpv4Address, TestSocketSet, VirtualStack,
    };

    tokio::time::timeout(TEST_TIMEOUT, async {
        // 1. Start a real backend echo server (the "port 0" backend).
        let backend_addr = spawn_echo_backend().await;

        // 2. Register a `*.devenv.local` service mapping VIP:5432 -> backend.
        let mut table = ServiceTable::new();
        let svc = table.register("my-db".to_string(), backend_addr, 5432, 0);
        let vip = svc.vip;
        let vip_v4 = TestIpv4Address::from_bytes(&vip.0);

        // 3. Spawn the real stack engine on one half of an in-memory device pair.
        let (stack_dev, mut client_dev) = MockDevice::pair();
        let stack = VirtualStack::spawn_with_device(stack_dev, table);

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

/// Root-gated real-TUN e2e. SKIPS cleanly when not root so unprivileged
/// `cargo test` passes; exercises the real overlay when run as root (`just e2e`).
#[tokio::test]
async fn real_tun_overlay() {
    // SAFETY: geteuid is always safe to call.
    let euid = unsafe { libc::geteuid() };
    if euid != 0 {
        println!("real_tun_overlay: skipped: requires root (euid={euid})");
        return;
    }

    use devenv_tunnel_daemon::net::overlay::{OverlayConfig, OverlayNetwork};

    println!("real_tun_overlay: running as root; bringing up real overlay");

    let result = tokio::time::timeout(TEST_TIMEOUT, async {
        // Use the embedded DNS on a high local port to avoid clashing with the
        // system resolver during the test.
        let mut config = OverlayConfig::default();
        config.dns_listen = "127.0.0.1:53000".parse().unwrap();

        let overlay = OverlayNetwork::start(config)
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
        let ip = dns_query_a("127.0.0.1:53000".parse().unwrap(), "rooted.devenv.local").await;
        assert!(ip.is_some(), "real overlay DNS did not resolve service");

        overlay.shutdown().await;
    })
    .await;

    result.expect("real_tun_overlay timed out");
    println!("real_tun_overlay: passed");
}
