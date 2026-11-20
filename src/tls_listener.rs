//! Separate the TLS listener's admission cutoff from accepted HTTP shutdown.

use axum::extract::connect_info::Connected;
use axum_server::{AddrListener, Address, Handle};
use std::{future::pending, io, net::SocketAddr, time::Duration};
use tokio::{
    net::{TcpListener, TcpStream},
    sync::{watch, Mutex},
};

use super::wait_for_shutdown;

#[derive(Clone, Copy)]
pub(super) struct TlsAddress(SocketAddr);

impl Address for TlsAddress {
    type Stream = TcpStream;
    type Listener = DrainableTlsListener;
}

impl Connected<TlsAddress> for SocketAddr {
    fn connect_info(remote: TlsAddress) -> Self {
        remote.0
    }
}

pub(super) struct DrainableTlsListener {
    // The address listener trait accepts through &self. Axum Server calls
    // accept serially, so only its accept future holds this guard.
    listener: Mutex<Option<TcpListener>>,
    local_addr: TlsAddress,
    shutdown_rx: watch::Receiver<bool>,
    drain_done_rx: watch::Receiver<bool>,
    handle: Handle<TlsAddress>,
    settle_budget: Duration,
}

impl DrainableTlsListener {
    pub(super) fn new(
        listener: TcpListener,
        shutdown_rx: watch::Receiver<bool>,
        drain_done_rx: watch::Receiver<bool>,
        handle: Handle<TlsAddress>,
        settle_budget: Duration,
    ) -> io::Result<Self> {
        let local_addr = TlsAddress(listener.local_addr()?);
        Ok(Self {
            listener: Mutex::new(Some(listener)),
            local_addr,
            shutdown_rx,
            drain_done_rx,
            handle,
            settle_budget,
        })
    }
}

impl AddrListener<TcpStream, TlsAddress> for DrainableTlsListener {
    async fn bind_to(_addr: TlsAddress) -> io::Result<Self> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "the TLS server requires a configured TCP listener",
        ))
    }

    async fn accept_stream(&self) -> io::Result<(TcpStream, TlsAddress)> {
        {
            let mut listener = self.listener.lock().await;
            if let Some(socket) = listener.as_ref() {
                tokio::select! {
                    biased;
                    _ = wait_for_shutdown(self.shutdown_rx.clone()) => {},
                    // Also release the listener if the drain task dies
                    // before it sends the admission cutoff.
                    _ = wait_for_shutdown(self.drain_done_rx.clone()) => {},
                    result = socket.accept() => {
                        return result.map(|(stream, addr)| (stream, TlsAddress(addr)));
                    }
                }
                drop(listener.take());
            }
        }

        // Established HTTP/2 connections can still carry WebSockets. Wait
        // for their coded closes before starting bounded HTTP shutdown.
        wait_for_shutdown(self.drain_done_rx.clone()).await;
        self.handle.graceful_shutdown(Some(self.settle_budget));

        // Axum Server retries accept errors. Let its handle stop the loop.
        pending().await
    }

    fn get_local_addr(&self) -> io::Result<TlsAddress> {
        Ok(self.local_addr)
    }
}
