//! End-to-end tests for the virtual overlay network.
//!
//! Two scenarios live here:
//!
//! * [`unprivileged_overlay_round_trip`] — runs in plain `cargo test` with NO
//!   root. It exercises the overlay components that are reachable through the
//!   daemon's public API: VIP allocation, the [`ServiceTable`], the embedded
//!   authoritative DNS server (`name.devenv.local` -> VIP), and a real tokio
//!   backend that the overlay would proxy to. The smoltcp user-space byte-proxy
//!   itself runs against a private in-crate stack engine and is covered by the
//!   unit test `net::stack::tests::vip_connect_proxies_to_backend`; the public
//!   API intentionally only exposes the real-TUN constructor, so the byte path
//!   is asserted there and the *wiring* (name -> VIP -> real backend) is
//!   asserted here.
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
        //    (The smoltcp byte-proxy across the VIP is covered by the in-crate
        //    unit test `vip_connect_proxies_to_backend`.)
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
