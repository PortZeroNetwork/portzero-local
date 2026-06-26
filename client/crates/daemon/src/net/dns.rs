//! Embedded authoritative DNS server for the overlay.
//!
//! Serves only A records for `<name>.portzero.local` -> virtual IP.
//! The server listens on 127.0.0.1:53 (or a configurable port) and is reached
//! via scoped OS configuration, never by hijacking the whole system resolver.
//!
//! We use hickory-proto for clean DNS message handling.

use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Arc;
#[cfg(target_os = "windows")]
use std::net::UdpSocket as StdUdpSocket;
#[cfg(target_os = "windows")]
use std::time::Duration;

use anyhow::Result;
use hickory_proto::op::{Message, MessageType, OpCode, ResponseCode};
use hickory_proto::rr::{DNSClass, Name, RData, Record, RecordType};
use hickory_proto::serialize::binary::{BinDecodable, BinEncodable};
use tokio::net::UdpSocket;
use tokio::sync::RwLock;

use crate::net::service_table::ServiceTable;
use crate::net::virtual_ip::{API_MANAGEMENT_VIP, MANAGEMENT_VIP};

/// Runs a simple UDP DNS server that answers A queries for the overlay.
pub struct OverlayDnsServer {
    services: Arc<RwLock<ServiceTable>>,
    listen_addr: SocketAddr,
}

impl OverlayDnsServer {
    pub fn new(services: Arc<RwLock<ServiceTable>>, listen_addr: SocketAddr) -> Self {
        Self {
            services,
            listen_addr,
        }
    }

    /// Run the DNS server until cancelled.
    pub async fn run(self) -> Result<()> {
        let sock = UdpSocket::bind(self.listen_addr).await?;
        tracing::info!("overlay DNS listening on {}", self.listen_addr);

        let mut buf = vec![0u8; 512];

        loop {
            let (len, src) = match sock.recv_from(&mut buf).await {
                Ok(v) => v,
                Err(e) => {
                    tracing::debug!("DNS recv error: {}", e);
                    continue;
                }
            };

            tracing::info!("overlay DNS query received from {}", src);
            let req_bytes = &buf[..len];
            let response = match self.handle_query(req_bytes).await {
                Some(resp) => resp,
                None => continue,
            };

            if let Err(e) = sock.send_to(&response, src).await {
                tracing::debug!("DNS send error to {}: {}", src, e);
            }
        }
    }

    /// Run the DNS server on a dedicated blocking thread.
    ///
    /// Windows startup/discovery can involve blocking OS inspection. Keeping DNS
    /// on a plain socket thread prevents those paths from starving name
    /// resolution once the resolver is configured.
    #[cfg(target_os = "windows")]
    pub fn run_blocking(self) -> Result<()> {
        let sock = StdUdpSocket::bind(self.listen_addr)?;
        sock.set_read_timeout(Some(Duration::from_secs(30)))?;
        tracing::info!("overlay DNS listening on {}", self.listen_addr);

        let mut buf = vec![0u8; 512];
        loop {
            let (len, src) = match sock.recv_from(&mut buf) {
                Ok(v) => v,
                Err(e)
                    if e.kind() == std::io::ErrorKind::WouldBlock
                        || e.kind() == std::io::ErrorKind::TimedOut =>
                {
                    tracing::debug!("overlay DNS receive wait timed out");
                    continue;
                }
                Err(e) => {
                    tracing::debug!("DNS recv error: {}", e);
                    continue;
                }
            };

            tracing::debug!("overlay DNS query received from {}", src);
            let req_bytes = &buf[..len];
            let services = self.services.blocking_read();
            let response = match handle_query_with_services(req_bytes, &services) {
                Some(resp) => resp,
                None => continue,
            };

            if let Err(e) = sock.send_to(&response, src) {
                tracing::debug!("DNS send error to {}: {}", src, e);
            }
        }
    }

    async fn handle_query(&self, data: &[u8]) -> Option<Vec<u8>> {
        let services = self.services.read().await;
        handle_query_with_services(data, &services)
    }
}

fn handle_query_with_services(data: &[u8], services: &ServiceTable) -> Option<Vec<u8>> {
    let msg = match Message::from_bytes(data) {
        Ok(m) => m,
        Err(_) => return None,
    };

    if msg.op_code() != OpCode::Query {
        return None;
    }

    let mut out = Message::new();
    out.set_id(msg.id());
    out.set_message_type(MessageType::Response);
    out.set_op_code(OpCode::Query);
    out.set_recursion_desired(msg.recursion_desired());
    out.set_recursion_available(true);
    out.set_authoritative(true);
    out.set_response_code(ResponseCode::NoError);

    for q in msg.queries() {
        let name = q.name().clone();
        if q.query_class() != DNSClass::IN {
            out.set_response_code(ResponseCode::NotImp);
            continue;
        }

        if q.query_type() == RecordType::A {
            if let Some(ip) = resolve_name_to_vip(&name, services) {
                let mut rec = Record::new();
                rec.set_name(name.clone());
                rec.set_rr_type(RecordType::A);
                rec.set_dns_class(DNSClass::IN);
                rec.set_ttl(5);
                rec.set_data(Some(RData::A(ip.into())));
                out.add_answer(rec);
                out.set_response_code(ResponseCode::NoError);
            } else if is_portzero_local(&name) {
                out.set_response_code(ResponseCode::NXDomain);
            }
        } else if is_portzero_local(&name) && resolve_name_to_vip(&name, services).is_none() {
            out.set_response_code(ResponseCode::NXDomain);
        }
    }

    // Add the original question
    for q in msg.queries() {
        out.add_query(q.clone());
    }

    out.to_bytes().ok()
}

fn is_portzero_local(name: &Name) -> bool {
    let s = name.to_ascii();
    s.ends_with(".portzero.local") || s == "portzero.local"
}

/// Given a DNS name like "my-db.portzero.local", look up the corresponding VIP.
fn resolve_name_to_vip(name: &Name, services: &ServiceTable) -> Option<Ipv4Addr> {
    let labels: Vec<_> = name
        .iter()
        .map(|l| std::str::from_utf8(l).unwrap_or(""))
        .collect();

    // Special case: "portzero.local" (2 labels) resolves to the management VIP
    // directly without a service table lookup, so it works before registration.
    if labels.len() == 2 && labels[0] == "portzero" && labels[1] == "local" {
        return Some(MANAGEMENT_VIP);
    }

    // Special case: "api.portzero.local" (3 labels) resolves to the management REST API VIP.
    if labels.len() == 3 && labels[0] == "api" && labels[1] == "portzero" && labels[2] == "local" {
        return Some(API_MANAGEMENT_VIP);
    }

    // Expect something like ["my-db", "portzero", "local"]
    if labels.len() < 3 {
        return None;
    }

    // The left-most label is our service name.
    let candidate = labels[0];

    // Must be under portzero.local
    if labels.len() >= 3
        && labels[labels.len() - 2] == "portzero"
        && labels[labels.len() - 1] == "local"
    {
        return services.get(candidate).map(|svc| {
            // smoltcp Ipv4Address can be converted via its Display or as_bytes in 0.11
            let b: [u8; 4] = svc.vip.0;
            std::net::Ipv4Addr::new(b[0], b[1], b[2], b[3])
        });
    }

    None
}

/// Helper to update the shared service table from outside.
pub async fn update_dns_services(
    dns_services: &Arc<RwLock<ServiceTable>>,
    new_table: ServiceTable,
) {
    let mut guard = dns_services.write().await;
    *guard = new_table;
}
