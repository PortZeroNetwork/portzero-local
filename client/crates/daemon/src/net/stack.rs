//! User-space TCP stack (smoltcp over TUN).
//!
//! This module implements the real async packet-routing loop for the virtual
//! overlay network:
//!
//! 1. Raw L3 (IP) packets are read from the TUN device and fed into a smoltcp
//!    [`Interface`] + [`SocketSet`].
//! 2. We listen on every registered service port across *all* virtual IPs
//!    (`AnyIP` + wildcard listen endpoints). When a client connects to
//!    `VIP:service_port` the smoltcp socket completes the TCP handshake entirely
//!    in user space (we never use OS sockets for the client side).
//! 3. Once a connection is established, we look up the [`NetworkService`] by the
//!    VIP the client targeted and open a `tokio::net::TcpStream` to the real
//!    ephemeral backend. Payload is then proxied bidirectionally between the
//!    smoltcp socket and the backend stream.
//!
//! ## Concurrency model
//!
//! smoltcp is synchronous and its sockets are not `Send`-friendly across `await`
//! points, so the entire smoltcp engine runs inside a single dedicated task (the
//! "stack loop"). That task owns the [`Interface`], the [`SocketSet`] and the
//! [`phy::Device`]. The loop:
//!
//! - drains inbound IP packets (from TUN, or a mock device in tests) into the
//!   device's RX queue,
//! - calls [`Interface::poll`] to advance every TCP socket,
//! - moves bytes between each established socket and its backend connection via
//!   per-connection mpsc channels (the actual blocking backend I/O lives in
//!   small async helper tasks so the loop never blocks on the network),
//! - flushes the device's TX queue back out to TUN.
//!
//! The public API ([`VirtualStack::spawn`], [`update_services`], [`shutdown`],
//! [`StackCommand`]) is unchanged so the rest of the system keeps compiling.

use std::collections::{HashMap, VecDeque};

use anyhow::Result;
use smoltcp::iface::{Config, Interface, SocketHandle, SocketSet};
use smoltcp::phy::{self, Device, DeviceCapabilities, Medium};
use smoltcp::socket::tcp;
use smoltcp::time::Instant;
use smoltcp::wire::{HardwareAddress, IpAddress, IpCidr, Ipv4Address, Ipv4Cidr};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use crate::net::service_table::ServiceTable;

/// MTU used for the virtual interface. Matches the default TUN MTU.
const STACK_MTU: usize = 1500;
/// Per-socket smoltcp send/recv buffer size.
const SOCKET_BUF: usize = 64 * 1024;
/// Chunk size used when shuttling bytes to/from the backend channels.
const PROXY_CHUNK: usize = 16 * 1024;
/// How often the stack loop wakes up even with no I/O, to drive smoltcp timers.
const POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(10);

pub enum StackCommand {
    UpdateServices(ServiceTable),
    Shutdown,
}

/// Handle to the running virtual stack. Cloneable senders are used to talk to
/// the dedicated stack task.
pub struct VirtualStack {
    cmd_tx: mpsc::Sender<StackCommand>,
    /// Handles to the TUN reader/writer tasks (only present for the real
    /// [`VirtualStack::spawn`] path; the test [`spawn_with_device`] path creates
    /// no TUN tasks and leaves these `None`).
    ///
    /// These must be aborted on [`shutdown`] so the reader task — which would
    /// otherwise stay parked forever in `tun_reader.read().await` — drops its
    /// half of the device. Once both halves drop, the TUN fd closes, the
    /// [`RouteGuard`] on the writer half runs, and the OS interface (`deven0`)
    /// disappears on a normal stop.
    reader_task: Option<JoinHandle<()>>,
    writer_task: Option<JoinHandle<()>>,
}

impl VirtualStack {
    /// Spawn the virtual stack driving the given TUN device.
    pub async fn spawn(
        tun: crate::net::tun_device::TunDevice,
        initial: ServiceTable,
    ) -> Result<Self> {
        let (cmd_tx, cmd_rx) = mpsc::channel::<StackCommand>(16);

        // Bridge the async TUN device into the synchronous smoltcp device via two
        // channels: inbound IP packets (TUN -> stack) and outbound IP packets
        // (stack -> TUN).
        let (inbound_tx, inbound_rx) = mpsc::channel::<Vec<u8>>(256);
        let (outbound_tx, mut outbound_rx) = mpsc::channel::<Vec<u8>>(256);

        let (tun_reader, tun_writer) = tun.split();

        // TUN reader task: read raw L3 packets and push them to the stack.
        let inbound_tx_reader = inbound_tx.clone();
        let reader_task = tokio::spawn(async move {
            let mut buf = vec![0u8; STACK_MTU + 4];
            loop {
                match tun_reader.read(&mut buf).await {
                    Ok(0) => break,
                    Ok(n) => {
                        if inbound_tx_reader.send(buf[..n].to_vec()).await.is_err() {
                            break;
                        }
                    }
                    Err(e) => {
                        tracing::warn!("TUN read error: {e}");
                        break;
                    }
                }
            }
            tracing::debug!("TUN reader task exiting");
        });

        // TUN writer task: drain outbound packets and write them to TUN.
        let writer_task = tokio::spawn(async move {
            while let Some(pkt) = outbound_rx.recv().await {
                if let Err(e) = tun_writer.write(&pkt).await {
                    tracing::warn!("TUN write error: {e}");
                    break;
                }
            }
            tracing::debug!("TUN writer task exiting");
        });

        let device = ChannelDevice::new(inbound_rx, outbound_tx);
        StackEngine::spawn(device, initial, cmd_rx);

        Ok(Self {
            cmd_tx,
            reader_task: Some(reader_task),
            writer_task: Some(writer_task),
        })
    }

    /// Spawn a stack over an arbitrary smoltcp device (used by tests with a mock
    /// in-memory device).
    // test-support: exposed for integration tests; not part of the stable API
    #[doc(hidden)]
    pub fn spawn_with_device<D>(device: D, initial: ServiceTable) -> Self
    where
        D: Device + Pumpable + Send + 'static,
    {
        let (cmd_tx, cmd_rx) = mpsc::channel::<StackCommand>(16);
        StackEngine::spawn(device, initial, cmd_rx);
        // The mock-device test path drives no real TUN, so there are no
        // reader/writer tasks to track or abort.
        Self {
            cmd_tx,
            reader_task: None,
            writer_task: None,
        }
    }

    pub async fn update_services(&self, table: ServiceTable) -> Result<()> {
        let _ = self.cmd_tx.send(StackCommand::UpdateServices(table)).await;
        Ok(())
    }

    pub async fn shutdown(&self) -> Result<()> {
        let _ = self.cmd_tx.send(StackCommand::Shutdown).await;
        // Abort the TUN reader/writer tasks so their halves of the device drop.
        // The reader task is otherwise parked forever in `read().await`, holding
        // the reader half (and, via the writer half's `RouteGuard`, the route and
        // fd). Aborting both lets the device fd close and the OS interface
        // (`deven0`) go away on a normal stop. `JoinHandle::abort` takes `&self`,
        // so aborting from `&self` here is fine. No-op on the test path where the
        // handles are `None`.
        if let Some(task) = self.reader_task.as_ref() {
            task.abort();
        }
        if let Some(task) = self.writer_task.as_ref() {
            task.abort();
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// smoltcp <-> tokio bridge device
// ---------------------------------------------------------------------------

/// A smoltcp [`Device`] backed by mpsc channels of raw IP packets.
///
/// `receive`/`transmit` are synchronous (as smoltcp requires); the async TUN
/// (or a test harness) pushes inbound packets and consumes outbound packets via
/// the channels. The stack loop calls [`ChannelDevice::pump_inbound`] before
/// each poll to move queued packets from the channel into the synchronous RX
/// queue.
pub struct ChannelDevice {
    inbound_rx: mpsc::Receiver<Vec<u8>>,
    outbound_tx: mpsc::Sender<Vec<u8>>,
    rx_queue: VecDeque<Vec<u8>>,
}

impl ChannelDevice {
    pub fn new(inbound_rx: mpsc::Receiver<Vec<u8>>, outbound_tx: mpsc::Sender<Vec<u8>>) -> Self {
        Self {
            inbound_rx,
            outbound_tx,
            rx_queue: VecDeque::new(),
        }
    }

    /// Move any packets currently available on the inbound channel into the
    /// synchronous RX queue. Returns the number of packets moved.
    fn pump_inbound(&mut self) -> usize {
        let mut moved = 0;
        while let Ok(pkt) = self.inbound_rx.try_recv() {
            self.rx_queue.push_back(pkt);
            moved += 1;
        }
        moved
    }
}

pub struct ChannelRxToken {
    buffer: Vec<u8>,
}

impl phy::RxToken for ChannelRxToken {
    fn consume<R, F>(mut self, f: F) -> R
    where
        F: FnOnce(&mut [u8]) -> R,
    {
        f(&mut self.buffer)
    }
}

pub struct ChannelTxToken {
    outbound_tx: mpsc::Sender<Vec<u8>>,
}

impl phy::TxToken for ChannelTxToken {
    fn consume<R, F>(self, len: usize, f: F) -> R
    where
        F: FnOnce(&mut [u8]) -> R,
    {
        let mut buf = vec![0u8; len];
        let result = f(&mut buf);
        // Best effort: if the consumer is gone, drop the packet.
        if let Err(e) = self.outbound_tx.try_send(buf) {
            tracing::trace!("dropping outbound packet: {e}");
        }
        result
    }
}

impl Device for ChannelDevice {
    type RxToken<'a> = ChannelRxToken;
    type TxToken<'a> = ChannelTxToken;

    fn receive(&mut self, _timestamp: Instant) -> Option<(Self::RxToken<'_>, Self::TxToken<'_>)> {
        let buffer = self.rx_queue.pop_front()?;
        Some((
            ChannelRxToken { buffer },
            ChannelTxToken {
                outbound_tx: self.outbound_tx.clone(),
            },
        ))
    }

    fn transmit(&mut self, _timestamp: Instant) -> Option<Self::TxToken<'_>> {
        Some(ChannelTxToken {
            outbound_tx: self.outbound_tx.clone(),
        })
    }

    fn capabilities(&self) -> DeviceCapabilities {
        let mut caps = DeviceCapabilities::default();
        caps.medium = Medium::Ip;
        caps.max_transmission_unit = STACK_MTU;
        caps
    }
}

// ---------------------------------------------------------------------------
// Per-connection backend proxy plumbing
// ---------------------------------------------------------------------------

/// State for a single proxied connection living inside the stack loop.
struct Connection {
    /// Bytes received from the backend, waiting to be pushed into the smoltcp
    /// socket's tx buffer.
    to_client: VecDeque<u8>,
    /// Sender to the backend writer task (bytes the client sent us).
    to_backend_tx: Option<mpsc::Sender<Vec<u8>>>,
    /// Receiver of bytes the backend sent back.
    from_backend_rx: mpsc::Receiver<Vec<u8>>,
    /// Set when the backend side has closed (EOF or error).
    backend_eof: bool,
    /// Set once we've initiated a smoltcp close toward the client.
    client_closing: bool,
}

/// Listening socket bookkeeping: one smoltcp listener per service port.
struct Listener {
    handle: SocketHandle,
    port: u16,
}

struct StackEngine<D: Device> {
    iface: Interface,
    sockets: SocketSet<'static>,
    device: D,
    services: ServiceTable,
    listeners: Vec<Listener>,
    connections: HashMap<SocketHandle, Connection>,
    cmd_rx: mpsc::Receiver<StackCommand>,
}

impl<D> StackEngine<D>
where
    D: Device + Pumpable + Send + 'static,
{
    fn spawn(mut device: D, initial: ServiceTable, cmd_rx: mpsc::Receiver<StackCommand>) {
        // The interface uses our virtual gateway IP and covers the entire
        // 10.254.0.0/16 block via a route so that AnyIP accepts every VIP.
        let gateway = crate::net::virtual_ip::gateway_ip();
        let config = Config::new(HardwareAddress::Ip);
        let mut iface = Interface::new(config, &mut device, Instant::now());

        let gw_addr = Ipv4Address::from_bytes(&gateway.octets());
        iface.update_ip_addrs(|addrs| {
            let _ = addrs.push(IpCidr::Ipv4(Ipv4Cidr::new(gw_addr, 16)));
        });
        // Accept packets addressed to any IP inside our subnet, not just the
        // gateway address.
        iface.set_any_ip(true);
        // A route pointing the whole subnet back at ourselves so AnyIP engages.
        let _ = iface.routes_mut().add_default_ipv4_route(gw_addr);

        let mut engine = StackEngine {
            iface,
            sockets: SocketSet::new(Vec::new()),
            device,
            services: ServiceTable::new(),
            listeners: Vec::new(),
            connections: HashMap::new(),
            cmd_rx,
        };
        engine.apply_services(initial);

        tokio::spawn(async move {
            engine.run().await;
        });
    }

    /// Replace the service table and (re)create listeners so there is one
    /// listening socket per distinct service port.
    fn apply_services(&mut self, table: ServiceTable) {
        let mut wanted_ports: Vec<u16> = table.all().map(|s| s.service_port).collect();
        wanted_ports.sort_unstable();
        wanted_ports.dedup();

        // Remove listeners whose port is no longer wanted.
        let mut keep: Vec<Listener> = Vec::new();
        for listener in self.listeners.drain(..) {
            if wanted_ports.contains(&listener.port) {
                keep.push(listener);
            } else {
                // Only remove if it's still purely a listener (not an accepted
                // connection that reused the handle — accepted sockets are moved
                // into `connections` and replaced with a fresh listener).
                let sock = self.sockets.get::<tcp::Socket>(listener.handle);
                if sock.is_listening() {
                    self.sockets.remove(listener.handle);
                }
            }
        }
        self.listeners = keep;

        // Add listeners for new ports.
        let existing: Vec<u16> = self.listeners.iter().map(|l| l.port).collect();
        for port in wanted_ports {
            if !existing.contains(&port) {
                if let Some(handle) = self.add_listener(port) {
                    self.listeners.push(Listener { handle, port });
                }
            }
        }

        self.services = table;
    }

    /// Create a new smoltcp listening socket on the given port across all VIPs.
    fn add_listener(&mut self, port: u16) -> Option<SocketHandle> {
        let socket = new_tcp_socket();
        let handle = self.sockets.add(socket);
        let sock = self.sockets.get_mut::<tcp::Socket>(handle);
        // `addr: None` => listen on this port across every local address (every VIP).
        match sock.listen(port) {
            Ok(()) => {
                tracing::debug!("listening on virtual port {port}");
                Some(handle)
            }
            Err(e) => {
                tracing::warn!("failed to listen on virtual port {port}: {e:?}");
                self.sockets.remove(handle);
                None
            }
        }
    }

    async fn run(mut self) {
        let mut ticker = tokio::time::interval(POLL_INTERVAL);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        loop {
            tokio::select! {
                cmd = self.cmd_rx.recv() => {
                    match cmd {
                        Some(StackCommand::UpdateServices(table)) => {
                            self.apply_services(table);
                        }
                        Some(StackCommand::Shutdown) | None => {
                            tracing::debug!("virtual stack shutting down");
                            break;
                        }
                    }
                }
                _ = ticker.tick() => {}
            }

            self.poll_once();
        }

        // Best-effort: abort all sockets on shutdown.
        for (_, sock) in self.sockets.iter_mut() {
            let smoltcp::socket::Socket::Tcp(tcp) = sock;
            tcp.abort();
        }
        let _ = self.iface.poll(Instant::now(), &mut self.device, &mut self.sockets);
    }

    /// One iteration: drain inbound packets, poll smoltcp, service every socket.
    fn poll_once(&mut self) {
        // Move any inbound TUN packets into the device RX queue. We only do this
        // for the real ChannelDevice; generic devices manage their own queues.
        self.pump_device();

        let timestamp = Instant::now();
        let _ = self
            .iface
            .poll(timestamp, &mut self.device, &mut self.sockets);

        self.accept_new_connections();
        self.service_connections();

        // Poll again so any bytes we just queued into sockets get flushed out.
        let _ = self
            .iface
            .poll(Instant::now(), &mut self.device, &mut self.sockets);
    }

    /// Detect listening sockets that just became connected, promote them to
    /// proxied connections, and replace them with a fresh listener so the port
    /// keeps accepting.
    fn accept_new_connections(&mut self) {
        let mut promotions: Vec<(usize, SocketHandle, u16)> = Vec::new();
        for (idx, listener) in self.listeners.iter().enumerate() {
            let sock = self.sockets.get::<tcp::Socket>(listener.handle);
            if sock.is_active() && !sock.is_listening() {
                promotions.push((idx, listener.handle, listener.port));
            }
        }

        for (idx, handle, port) in promotions {
            // Resolve which VIP the client connected to.
            let local = self
                .sockets
                .get::<tcp::Socket>(handle)
                .local_endpoint();

            let backend = match local {
                Some(ep) => match ep.addr {
                    IpAddress::Ipv4(v4) => self
                        .services
                        .resolve_for_connect(v4, port)
                        .map(|s| s.real_addr),
                    #[allow(unreachable_patterns)]
                    _ => None,
                },
                None => None,
            };

            let Some(real_addr) = backend else {
                tracing::warn!(
                    "no backend for accepted connection on port {port} (local={local:?}); aborting"
                );
                self.sockets.get_mut::<tcp::Socket>(handle).abort();
                // Replace listener so the port keeps working.
                self.replace_listener(idx, port);
                continue;
            };

            tracing::debug!("accepted virtual connection on port {port} -> backend {real_addr}");

            // Spawn backend connection plumbing.
            let (to_backend_tx, to_backend_rx) = mpsc::channel::<Vec<u8>>(16);
            let (from_backend_tx, from_backend_rx) = mpsc::channel::<Vec<u8>>(16);
            spawn_backend(real_addr, to_backend_rx, from_backend_tx);

            self.connections.insert(
                handle,
                Connection {
                    to_client: VecDeque::new(),
                    to_backend_tx: Some(to_backend_tx),
                    from_backend_rx,
                    backend_eof: false,
                    client_closing: false,
                },
            );

            // The old handle is now a live connection; install a fresh listener
            // for the port.
            self.replace_listener(idx, port);
        }
    }

    /// Replace the listener at `idx` (whose socket was consumed by an accepted
    /// connection) with a brand new listening socket on the same port.
    fn replace_listener(&mut self, idx: usize, port: u16) {
        if let Some(handle) = self.add_listener(port) {
            if idx < self.listeners.len() {
                self.listeners[idx] = Listener { handle, port };
            } else {
                self.listeners.push(Listener { handle, port });
            }
        } else if idx < self.listeners.len() {
            self.listeners.remove(idx);
        }
    }

    /// Move bytes between each established smoltcp socket and its backend.
    fn service_connections(&mut self) {
        let handles: Vec<SocketHandle> = self.connections.keys().copied().collect();

        for handle in handles {
            // 1. Drain bytes the client sent into the backend channel.
            self.drain_client_to_backend(handle);
            // 2. Pull bytes the backend sent us into the to_client buffer.
            self.pull_backend_bytes(handle);
            // 3. Push queued backend bytes into the smoltcp tx buffer.
            self.push_to_client(handle);
            // 4. Handle close conditions and cleanup.
            self.handle_close(handle);
        }

        // Remove finished connections.
        self.connections.retain(|_, c| !c.is_finished());
    }

    fn drain_client_to_backend(&mut self, handle: SocketHandle) {
        let Some(conn) = self.connections.get_mut(&handle) else {
            return;
        };
        let sock = self.sockets.get_mut::<tcp::Socket>(handle);

        while sock.can_recv() {
            let Some(tx) = conn.to_backend_tx.as_ref() else {
                break;
            };
            // Respect backend channel capacity to provide backpressure.
            let permit = match tx.try_reserve() {
                Ok(p) => p,
                Err(_) => break, // channel full or closed; try later
            };
            let mut chunk = vec![0u8; PROXY_CHUNK];
            match sock.recv_slice(&mut chunk) {
                Ok(0) => break,
                Ok(n) => {
                    chunk.truncate(n);
                    permit.send(chunk);
                }
                Err(_) => break,
            }
        }

        // If the client closed its send side, signal EOF to the backend by
        // dropping the sender. We only treat this as a half-close once the
        // connection has progressed past the handshake into a state where the
        // remote has actually sent (or will send) a FIN. `may_recv()` is false
        // both before establishment and after the peer's FIN, so we additionally
        // require that we have nothing left to read.
        let client_half_closed = matches!(
            sock.state(),
            tcp::State::CloseWait
                | tcp::State::LastAck
                | tcp::State::Closed
                | tcp::State::Closing
                | tcp::State::TimeWait
        );
        if client_half_closed && conn.to_backend_tx.is_some() && !sock.can_recv() {
            conn.to_backend_tx = None;
        }
    }

    fn pull_backend_bytes(&mut self, handle: SocketHandle) {
        let Some(conn) = self.connections.get_mut(&handle) else {
            return;
        };
        loop {
            match conn.from_backend_rx.try_recv() {
                Ok(bytes) => conn.to_client.extend(bytes),
                Err(mpsc::error::TryRecvError::Empty) => break,
                Err(mpsc::error::TryRecvError::Disconnected) => {
                    conn.backend_eof = true;
                    break;
                }
            }
        }
    }

    fn push_to_client(&mut self, handle: SocketHandle) {
        let Some(conn) = self.connections.get_mut(&handle) else {
            return;
        };
        let sock = self.sockets.get_mut::<tcp::Socket>(handle);
        while !conn.to_client.is_empty() && sock.can_send() {
            let (front, _) = conn.to_client.as_slices();
            if front.is_empty() {
                conn.to_client.make_contiguous();
                continue;
            }
            match sock.send_slice(front) {
                Ok(0) => break,
                Ok(n) => {
                    conn.to_client.drain(..n);
                }
                Err(_) => break,
            }
        }
    }

    fn handle_close(&mut self, handle: SocketHandle) {
        let Some(conn) = self.connections.get_mut(&handle) else {
            return;
        };
        let sock = self.sockets.get_mut::<tcp::Socket>(handle);

        // Backend finished and we've flushed everything to the client: close the
        // client-facing side.
        if conn.backend_eof
            && conn.to_client.is_empty()
            && !conn.client_closing
            && sock.may_send()
        {
            sock.close();
            conn.client_closing = true;
        }
    }

    /// Pump packets into the device. The real [`ChannelDevice`] moves queued
    /// inbound packets from its channel into its synchronous RX queue here;
    /// self-contained mock devices no-op.
    fn pump_device(&mut self) {
        self.device.pump();
    }
}

/// Allow the engine to ask a device to move any externally-queued packets into
/// its synchronous RX queue before a poll. Real channel-backed devices use this;
/// self-contained mock devices can rely on the default no-op.
pub trait Pumpable {
    fn pump(&mut self) {}
}

impl Pumpable for ChannelDevice {
    fn pump(&mut self) {
        self.pump_inbound();
    }
}

impl Connection {
    fn is_finished(&self) -> bool {
        // A connection is done once the backend is gone, everything is flushed,
        // and we've closed the client side.
        self.backend_eof
            && self.to_client.is_empty()
            && self.client_closing
            && self.to_backend_tx.is_none()
    }
}

/// Build a new TCP socket with our standard buffer sizes.
// test-support: exposed for integration tests; not part of the stable API
#[doc(hidden)]
pub fn new_tcp_socket() -> tcp::Socket<'static> {
    let rx = tcp::SocketBuffer::new(vec![0u8; SOCKET_BUF]);
    let tx = tcp::SocketBuffer::new(vec![0u8; SOCKET_BUF]);
    tcp::Socket::new(rx, tx)
}

/// Spawn the async tasks that own the real backend `TcpStream`. One task reads
/// from the backend and forwards to the stack; the same task writes client bytes
/// to the backend.
fn spawn_backend(
    real_addr: std::net::SocketAddr,
    mut to_backend_rx: mpsc::Receiver<Vec<u8>>,
    from_backend_tx: mpsc::Sender<Vec<u8>>,
) {
    tokio::spawn(async move {
        let stream = match tokio::net::TcpStream::connect(real_addr).await {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!("backend connect to {real_addr} failed: {e}");
                // Dropping from_backend_tx signals EOF/abort to the stack.
                return;
            }
        };
        let _ = stream.set_nodelay(true);
        let (mut rd, mut wr) = stream.into_split();

        // Backend -> client
        let reader = tokio::spawn(async move {
            use tokio::io::AsyncReadExt;
            let mut buf = vec![0u8; PROXY_CHUNK];
            loop {
                match rd.read(&mut buf).await {
                    Ok(0) => break,
                    Ok(n) => {
                        if from_backend_tx.send(buf[..n].to_vec()).await.is_err() {
                            break;
                        }
                    }
                    Err(e) => {
                        tracing::trace!("backend read error from {real_addr}: {e}");
                        break;
                    }
                }
            }
            // Dropping from_backend_tx here signals backend EOF to the stack.
        });

        // Client -> backend
        let writer = tokio::spawn(async move {
            use tokio::io::AsyncWriteExt;
            while let Some(chunk) = to_backend_rx.recv().await {
                if wr.write_all(&chunk).await.is_err() {
                    break;
                }
            }
            let _ = wr.shutdown().await;
        });

        let _ = reader.await;
        let _ = writer.await;
        tracing::trace!("backend proxy for {real_addr} finished");
    });
}

// ---------------------------------------------------------------------------
// Test-support scaffolding (in-memory device + client interface)
// ---------------------------------------------------------------------------
//
// The following items are exposed (always compiled, `#[doc(hidden)] pub`) so
// that external integration tests in `tests/` — which compile against this
// crate as a normal dependency and therefore cannot see `#[cfg(test)]` code —
// can drive the real smoltcp `StackEngine` against an in-memory device. They
// are NOT part of the stable public API.

// test-support: exposed for integration tests; not part of the stable API
#[doc(hidden)]
pub use smoltcp::iface::SocketSet as TestSocketSet;
// test-support: exposed for integration tests; not part of the stable API
#[doc(hidden)]
pub use smoltcp::socket::tcp as test_tcp;
// test-support: exposed for integration tests; not part of the stable API
#[doc(hidden)]
pub use smoltcp::time::Instant as TestInstant;
// test-support: exposed for integration tests; not part of the stable API
#[doc(hidden)]
pub use smoltcp::wire::{IpAddress as TestIpAddress, Ipv4Address as TestIpv4Address};

use std::sync::{Arc, Mutex};

/// An in-memory loopback smoltcp device used to drive the stack from a test
/// "client" smoltcp interface. Packets written by one side appear on the
/// other.
// test-support: exposed for integration tests; not part of the stable API
#[doc(hidden)]
#[derive(Clone)]
pub struct MockDevice {
    // Packets destined for THIS device's RX (written by the peer).
    rx: Arc<Mutex<VecDeque<Vec<u8>>>>,
    // Packets transmitted by THIS device (read by the peer).
    tx: Arc<Mutex<VecDeque<Vec<u8>>>>,
}

// test-support: exposed for integration tests; not part of the stable API
#[doc(hidden)]
impl MockDevice {
    /// Create a connected pair of devices.
    pub fn pair() -> (MockDevice, MockDevice) {
        let a_to_b = Arc::new(Mutex::new(VecDeque::new()));
        let b_to_a = Arc::new(Mutex::new(VecDeque::new()));
        let a = MockDevice {
            rx: b_to_a.clone(),
            tx: a_to_b.clone(),
        };
        let b = MockDevice {
            rx: a_to_b,
            tx: b_to_a,
        };
        (a, b)
    }
}

// test-support: exposed for integration tests; not part of the stable API
#[doc(hidden)]
pub struct MockRxToken(Vec<u8>);
impl phy::RxToken for MockRxToken {
    fn consume<R, F>(mut self, f: F) -> R
    where
        F: FnOnce(&mut [u8]) -> R,
    {
        f(&mut self.0)
    }
}
// test-support: exposed for integration tests; not part of the stable API
#[doc(hidden)]
pub struct MockTxToken(Arc<Mutex<VecDeque<Vec<u8>>>>);
impl phy::TxToken for MockTxToken {
    fn consume<R, F>(self, len: usize, f: F) -> R
    where
        F: FnOnce(&mut [u8]) -> R,
    {
        let mut buf = vec![0u8; len];
        let r = f(&mut buf);
        self.0.lock().unwrap().push_back(buf);
        r
    }
}

impl Device for MockDevice {
    type RxToken<'a> = MockRxToken;
    type TxToken<'a> = MockTxToken;

    fn receive(&mut self, _t: Instant) -> Option<(Self::RxToken<'_>, Self::TxToken<'_>)> {
        let pkt = self.rx.lock().unwrap().pop_front()?;
        Some((MockRxToken(pkt), MockTxToken(self.tx.clone())))
    }

    fn transmit(&mut self, _t: Instant) -> Option<Self::TxToken<'_>> {
        Some(MockTxToken(self.tx.clone()))
    }

    fn capabilities(&self) -> DeviceCapabilities {
        let mut caps = DeviceCapabilities::default();
        caps.medium = Medium::Ip;
        caps.max_transmission_unit = STACK_MTU;
        caps
    }
}

// The mock device's queues are self-contained, so pumping is a no-op.
impl Pumpable for MockDevice {}

/// Build a client-side smoltcp [`Interface`] bound to `client_ip` and routed at
/// the virtual gateway, suitable for connecting to a VIP through a [`MockDevice`].
// test-support: exposed for integration tests; not part of the stable API
#[doc(hidden)]
pub fn client_iface(device: &mut MockDevice, client_ip: Ipv4Address) -> Interface {
    let config = Config::new(HardwareAddress::Ip);
    let mut iface = Interface::new(config, device, Instant::now());
    iface.update_ip_addrs(|addrs| {
        let _ = addrs.push(IpCidr::Ipv4(Ipv4Cidr::new(client_ip, 16)));
    });
    let gw = crate::net::virtual_ip::gateway_ip();
    let _ = iface
        .routes_mut()
        .add_default_ipv4_route(Ipv4Address::from_bytes(&gw.octets()));
    iface
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    /// End-to-end: a smoltcp "client" connects to VIP:port, the stack accepts via
    /// smoltcp, proxies to a real tokio echo backend, and data flows both ways.
    #[tokio::test]
    async fn vip_connect_proxies_to_backend() {
        let _ = tracing_subscriber_try_init();

        // 1. Start a real backend echo server.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let backend_addr = listener.local_addr().unwrap();
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

        // 2. Build the service table mapping a VIP:5432 -> backend.
        let mut table = ServiceTable::new();
        let svc = table.register("my-db".to_string(), backend_addr, 5432, 0);
        let vip = svc.vip;

        // 3. Spawn the stack on one half of a mock device pair.
        let (stack_dev, mut client_dev) = MockDevice::pair();
        let stack = VirtualStack::spawn_with_device(stack_dev, table);

        // 4. Build a client smoltcp interface on the other half and connect to
        //    VIP:5432 from a client IP in the same subnet.
        let client_ip = Ipv4Address::new(10, 254, 9, 9);
        let mut client_iface = client_iface(&mut client_dev, client_ip);
        let mut client_sockets = SocketSet::new(Vec::new());
        let client_handle = client_sockets.add(new_tcp_socket());

        {
            let sock = client_sockets.get_mut::<tcp::Socket>(client_handle);
            let cx = client_iface.context();
            sock.connect(cx, (IpAddress::Ipv4(vip), 5432u16), (client_ip, 49000u16))
                .unwrap();
        }

        // 5. Drive both sides until the connection is established and echo works.
        let payload = b"hello virtual overlay";
        let mut sent = false;
        let mut received = Vec::new();

        for _ in 0..2000 {
            client_iface.poll(Instant::now(), &mut client_dev, &mut client_sockets);

            let sock = client_sockets.get_mut::<tcp::Socket>(client_handle);
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

            // Give the stack task time to run its poll loop and the backend to
            // echo.
            tokio::time::sleep(std::time::Duration::from_millis(2)).await;
        }

        assert!(sent, "client never reached a sendable state (handshake failed)");
        assert_eq!(
            received, payload,
            "echoed payload did not match what was sent"
        );

        stack.shutdown().await.unwrap();
    }

    /// Shutdown should stop the stack task cleanly without panicking and reject
    /// further use gracefully.
    #[tokio::test]
    async fn shutdown_is_graceful() {
        let (stack_dev, _client_dev) = MockDevice::pair();
        let table = ServiceTable::new();
        let stack = VirtualStack::spawn_with_device(stack_dev, table);

        stack.shutdown().await.unwrap();
        // Give the task a moment to exit.
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        // Sending another command after shutdown must not panic; it may simply be
        // dropped if the receiver is gone.
        let _ = stack.update_services(ServiceTable::new()).await;
    }

    /// Updating services adds listeners for new ports without disrupting the
    /// stack.
    #[tokio::test]
    async fn update_services_adds_ports() {
        let (stack_dev, _client_dev) = MockDevice::pair();
        let stack = VirtualStack::spawn_with_device(stack_dev, ServiceTable::new());

        let mut table = ServiceTable::new();
        table.register(
            "svc-a".to_string(),
            "127.0.0.1:1".parse().unwrap(),
            8080,
            0,
        );
        table.register(
            "svc-b".to_string(),
            "127.0.0.1:2".parse().unwrap(),
            9090,
            0,
        );
        stack.update_services(table).await.unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        stack.shutdown().await.unwrap();
    }

    fn tracing_subscriber_try_init() {}
}
