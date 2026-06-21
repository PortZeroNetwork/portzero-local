//! Virtual overlay network: "Port 0 + Virtual Mesh" implementation.
//!
//! Core idea:
//! - Processes/containers declare `DEVENV_NETWORK=<name>` and bind port 0.
//! - We discover the real ephemeral host port (e.g. 127.0.0.1:32768).
//! - We assign a stable virtual IP from 10.254.0.0/16 (e.g. 10.254.0.5).
//! - We serve DNS for `<name>.devenv.local` -> virtual IP (scoped, not system-wide).
//! - A TUN device + smoltcp user-space stack receives connections to the VIP:service_port.
//! - On TCP connect, we complete the handshake in user space, then transparently proxy
//!   the byte stream to the real discovered addr using a regular tokio TcpStream.
//!
//! This makes `my-db.devenv.local:5432` work even though the DB actually listens on
//! a random ephemeral port.

pub mod dns;
pub mod overlay;
pub mod service_table;
pub mod stack;
pub mod tun_device;
pub mod virtual_ip;

pub use service_table::{NetworkService, ServiceTable};
pub use virtual_ip::VirtualIpAllocator;