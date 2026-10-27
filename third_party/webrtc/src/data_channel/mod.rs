//! DataChannel API
//!
//! This module provides the [`DataChannel`] trait and the [`DataChannelEvent`] enum, which are used
//! to establish bidirectional, low-latency, peer-to-peer data channels.
//!
//! # Concepts
//!
//! *   **[`DataChannel`]**: Represents a WebRTC data channel. It is created via
//!     [`PeerConnection::create_data_channel`](crate::peer_connection::PeerConnection::create_data_channel) or received via the
//!     [`PeerConnectionEventHandler::on_data_channel`](crate::peer_connection::PeerConnectionEventHandler::on_data_channel) callback.
//! *   **Event Polling**: Unlike the callback-heavy design of older versions, events (like opening,
//!     closing, receiving messages, or errors) are fetched by calling the asynchronous
//!     [`DataChannel::poll`] method in a loop.
//!
//! # Examples
//!
//! ## Sending and Receiving Data
//!
//! ```no_run
//! # use webrtc::data_channel::{DataChannel, DataChannelEvent};
//! # use std::sync::Arc;
//! # async fn handle_data_channel(dc: Arc<dyn DataChannel>) -> webrtc::error::Result<()> {
//! // Poll for events in a loop
//! while let Some(event) = dc.poll().await {
//!     match event {
//!         DataChannelEvent::OnOpen => {
//!             println!("Data channel opened!");
//!             dc.send_text("Hello, peer!").await?;
//!         }
//!         DataChannelEvent::OnMessage(msg) => {
//!             if let Some(text) = String::from_utf8(msg.data.to_vec()).ok() {
//!                 println!("Received text message: {}", text);
//!             }
//!         }
//!         DataChannelEvent::OnClose => {
//!             println!("Data channel closed");
//!             break;
//!         }
//!         _ => {}
//!     }
//! }
//! # Ok(())
//! # }
//! ```

use crate::peer_connection::PeerConnectionRef;
use crate::runtime::{Mutex, Receiver};
use bytes::BytesMut;
use futures::FutureExt;
use rtc::shared::error::{Error, Result};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

pub use rtc::data_channel::{
    RTCDataChannelId, RTCDataChannelInit, RTCDataChannelMessage, RTCDataChannelState,
};

/// Object-safe trait exposing all public DataChannel operations.
///
/// This allows `on_data_channel` in `PeerConnectionEventHandler` to receive a
/// `Arc<dyn DataChannel>` without the event handler trait itself needing to
/// be generic over the interceptor type `I`.
///
/// # Why the accessors return `Result`
///
/// Unlike the browser API — and unlike the Sans-I/O core, where these are plain field reads —
/// every `async` method here is a round-trip to the background driver that owns the connection
/// state. So even a read as simple as [`label`](Self::label) is fallible: it returns
/// [`Error::ErrDataChannelClosed`] once the channel or its peer connection is gone. That is the
/// only way these accessors fail, and it is not repeated on each one below.
///
/// [`id`](Self::id) is the exception: it is a plain, infallible read, because the handle caches
/// it. See the module docs for which methods are `async` and why.
#[async_trait::async_trait]
pub trait DataChannel: crate::sealed::Sealed + Send + Sync + 'static {
    /// Returns the label of this data channel.
    ///
    /// Labels are not required to be unique across channels.
    async fn label(&self) -> Result<String>;
    /// Returns whether this data channel guarantees in-order delivery.
    async fn ordered(&self) -> Result<bool>;
    /// Returns the maximum packet lifetime in milliseconds, or `None` if the channel is
    /// reliable.
    async fn max_packet_life_time(&self) -> Result<Option<u16>>;
    /// Returns the maximum number of retransmissions, or `None` if the channel is reliable.
    async fn max_retransmits(&self) -> Result<Option<u16>>;
    /// Returns the subprotocol name configured for this data channel, or the empty string if
    /// none was negotiated.
    async fn protocol(&self) -> Result<String>;
    /// Returns whether this data channel was negotiated out-of-band by the application
    /// (`true`), or established in-band over DCEP (`false`).
    async fn negotiated(&self) -> Result<bool>;
    /// Returns the identifier for this data channel.
    ///
    /// The value is initially null — which is what is returned if the id was not provided
    /// at channel creation time and the DTLS role of the SCTP transport has not yet been
    /// negotiated. Otherwise it returns the id that was either selected by the application
    /// or generated. Once set to a non-null value it does not change.
    fn id(&self) -> RTCDataChannelId;
    /// Returns the current state of this data channel.
    async fn ready_state(&self) -> Result<RTCDataChannelState>;
    /// Returns the threshold at which the buffered amount is considered to be *high*.
    ///
    /// When the buffered amount increases from below this threshold to equal or above it,
    /// the [`OnBufferedAmountHigh`](DataChannelEvent::OnBufferedAmountHigh) event fires.
    /// Initially `u32::MAX` on each new channel; the application may change it at any time
    /// with [`set_buffered_amount_high_threshold`](Self::set_buffered_amount_high_threshold).
    async fn buffered_amount_high_threshold(&self) -> Result<u32>;
    /// Sets the buffered amount high threshold in bytes.
    async fn set_buffered_amount_high_threshold(&self, threshold: u32) -> Result<()>;
    /// Returns the threshold at which the buffered amount is considered to be *low*.
    ///
    /// When the buffered amount decreases from above this threshold to equal or below it,
    /// the [`OnBufferedAmountLow`](DataChannelEvent::OnBufferedAmountLow) event fires.
    /// Initially `0` on each new channel; the application may change it at any time with
    /// [`set_buffered_amount_low_threshold`](Self::set_buffered_amount_low_threshold).
    async fn buffered_amount_low_threshold(&self) -> Result<u32>;
    /// Sets the buffered amount low threshold in bytes.
    async fn set_buffered_amount_low_threshold(&self, threshold: u32) -> Result<()>;
    /// Returns the send-buffer bytes charged by [`send`](Self::send)/[`send_text`](Self::send_text)
    /// that SCTP has not yet released (acknowledged or abandoned), including bytes queued in the send
    /// pipeline (unlike `bufferedAmount`, which counts only post-packetization).
    /// Empty messages charge one filler byte; application statistics exclude it.
    ///
    /// Has a default so that adding it did not have to touch every implementation; the
    /// built-in channel overrides it with the real counter that drives send back-pressure.
    /// (This trait is sealed, so the only implementations are in-crate — see `docs/semver.md`.)
    async fn outstanding_bytes(&self) -> Result<usize> {
        Ok(0)
    }
    /// Sends raw binary data on this data channel.
    ///
    /// Queues the message and returns; it never waits for the peer to acknowledge. When a
    /// send-buffer limit is configured
    /// ([`PeerConnectionBuilder::with_data_channel_send_buffer_limit`]) it first **blocks**
    /// until the channel's outstanding bytes are below the limit — mirroring
    /// `tokio::mpsc::Sender::send`. With no limit (the default) it never blocks, like the
    /// browser `RTCDataChannel.send()`. For a non-blocking send that fails fast when the
    /// buffer is full, use [`try_send`](Self::try_send).
    ///
    /// # Errors
    ///
    /// - [`Error::ErrDataChannelClosed`] if the channel/connection is closed or closing —
    ///   including a caller blocked awaiting capacity when `close()`/`Drop` runs.
    ///
    /// [`PeerConnectionBuilder::with_data_channel_send_buffer_limit`]: crate::peer_connection::PeerConnectionBuilder::with_data_channel_send_buffer_limit
    ///
    /// # Examples
    ///
    /// ```no_run
    /// # use bytes::BytesMut;
    /// # use webrtc::error::Result;
    /// # use webrtc::data_channel::DataChannel;
    /// # use std::sync::Arc;
    /// # async fn example(dc: Arc<dyn DataChannel>) -> Result<()> {
    /// dc.send(BytesMut::from(&b"Hello, WebRTC!"[..])).await?;
    /// # Ok(())
    /// # }
    /// ```
    async fn send(&self, data: BytesMut) -> Result<()>;
    /// Sends text data on this data channel.
    ///
    /// Blocking/non-blocking behaviour and error contract match [`send`](Self::send): with a
    /// configured send-buffer limit it blocks until the buffer is below the limit, and
    /// returns [`Error::ErrDataChannelClosed`] on a closing/closed channel. Use
    /// [`try_send_text`](Self::try_send_text) for the non-blocking variant.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// # use webrtc::error::Result;
    /// # use webrtc::data_channel::DataChannel;
    /// # use std::sync::Arc;
    /// # async fn example(dc: Arc<dyn DataChannel>) -> Result<()> {
    /// dc.send_text("Hello, WebRTC!").await?;
    /// # Ok(())
    /// # }
    /// ```
    async fn send_text(&self, text: &str) -> Result<()>;
    /// Waits until this channel can accept more data — its outstanding send bytes are
    /// below the configured send-buffer limit.
    ///
    /// Resolves immediately when no limit is configured (the default) or the buffer is
    /// already below it. Returns [`Error::ErrDataChannelClosed`] if the channel or
    /// connection is closing. This is the await-for-capacity primitive that blocking
    /// [`send`](Self::send) uses internally; call it directly to pace your own sends.
    ///
    /// Level-triggered, not a reserved permit: with multiple concurrent senders on one
    /// channel the limit is a soft bound (each admitted sender may add one in-flight
    /// message over it) — the same semantics as `bufferedAmount`-based flow control.
    ///
    /// Defaults to `Ok(())`; the built-in channel overrides it. This trait is sealed, so
    /// the default exists for in-crate implementations, not downstream ones.
    async fn writable(&self) -> Result<()> {
        Ok(())
    }
    /// Non-blocking [`send`](Self::send): enqueues `data` and returns at once, or fails
    /// fast with [`Error::ErrSendBufferFull`] when a send-buffer limit is configured and
    /// this message would push the channel's outstanding bytes past it. Never waits for
    /// capacity. Mirrors `tokio::mpsc::Sender::try_send`.
    ///
    /// `data` is **consumed** on rejection, so retain a clone if you intend to retry.
    ///
    /// Defaults to delegating to [`send`](Self::send) — correct for implementors that do
    /// not impose a blocking limit, since there `send` is already non-blocking.
    async fn try_send(&self, data: BytesMut) -> Result<()> {
        self.send(data).await
    }
    /// Non-blocking [`send_text`](Self::send_text); see [`try_send`](Self::try_send).
    ///
    /// Defaults to delegating to [`send_text`](Self::send_text).
    async fn try_send_text(&self, text: &str) -> Result<()> {
        self.send_text(text).await
    }
    /// Polls for the next event on this data channel.
    ///
    /// Resolves when the next [`DataChannelEvent`] is available, and returns `None` once the
    /// channel is closed and no events remain — so a `while let Some(event) = dc.poll().await`
    /// loop terminates on its own.
    async fn poll(&self) -> Option<DataChannelEvent>;
    /// Closes the data channel.
    ///
    /// Moves the channel to `Closing` and starts the DCEP/SCTP teardown; closing an
    /// already-closed channel succeeds. Senders blocked in [`send`](Self::send) or
    /// [`writable`](Self::writable) awaiting capacity are woken with
    /// [`Error::ErrDataChannelClosed`].
    async fn close(&self) -> Result<()>;
}

/// Events that can occur on a [`DataChannel`].
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum DataChannelEvent {
    /// Data channel has opened and is ready to send/receive data.
    ///
    /// This event is fired when the data channel transitions to the "open" state.
    /// Data can now be sent through the channel.
    OnOpen,

    /// An error occurred on the data channel.
    ///
    /// This event is fired when an error is encountered. The channel may still
    /// be usable depending on the error type.
    OnError,

    /// Data channel is closing.
    ///
    /// This event is fired when the channel begins the closing process.
    /// The channel is transitioning to the "closing" state.
    OnClosing,

    /// Data channel has closed.
    ///
    /// This event is fired when the channel is fully closed and no longer usable.
    /// No more data can be sent or received.
    OnClose,

    /// Buffered amount dropped below the low-water mark.
    ///
    /// This event is fired when the amount of buffered outgoing data drops
    /// below the threshold set by `set_buffered_amount_low_threshold()`.
    /// This indicates it's safe to send more data without causing excessive buffering.
    ///
    /// Use this event to implement flow control and prevent memory exhaustion.
    OnBufferedAmountLow,

    /// Buffered amount exceeded the high-water mark (implementation-specific).
    ///
    /// This is a non-standard event that can be used to detect when too much
    /// data is being buffered. Applications should pause sending when this fires.
    OnBufferedAmountHigh,

    /// OnMessage with a binary message arrival over the sctp transport from a remote peer.
    ///
    /// OnMessage can currently receive messages up to 16384 bytes
    /// in size. Check out the detach API if you want to use larger
    /// message sizes. Note that browser support for larger messages
    /// is also limited.
    OnMessage(RTCDataChannelMessage),
}

/// Concrete async data channel implementation (generic over interceptor type).
///
/// This wraps a data channel and provides async send/receive APIs.
pub(crate) struct DataChannelImpl {
    /// Unique identifier for this data channel
    id: RTCDataChannelId,

    /// Inner PeerConnection Reference
    inner: Arc<PeerConnectionRef>,

    /// event receiver
    evt_rx: Mutex<Receiver<DataChannelEvent>>,
}

impl DataChannelImpl {
    /// Create a new data channel wrapper
    pub(crate) fn new(
        id: RTCDataChannelId,
        inner: Arc<PeerConnectionRef>,
        evt_rx: Receiver<DataChannelEvent>,
    ) -> Self {
        Self {
            id,
            inner,
            evt_rx: Mutex::new(evt_rx),
        }
    }

    async fn wait_for_send_buffer(&self) -> Result<()> {
        if self.inner.closing.load(Ordering::Acquire) {
            return Err(Error::ErrDataChannelClosed);
        }
        /* Unbounded sends check channel state under the enqueue lock. */
        if self.inner.data_channel_send_buffer_limit != usize::MAX {
            self.writable().await?;
        }
        Ok(())
    }
}

impl Drop for DataChannelImpl {
    fn drop(&mut self) {
        if let Some(mut data_channels) = self.inner.data_channel_events_tx.try_lock() {
            data_channels.remove(&self.id);
        }
    }
}

impl crate::sealed::Sealed for DataChannelImpl {}

#[async_trait::async_trait]
impl DataChannel for DataChannelImpl {
    // Documented on `DataChannel::label`.
    async fn label(&self) -> Result<String> {
        let mut peer_connection = self.inner.core.lock().await;

        Ok(peer_connection
            .data_channel(self.id)
            .ok_or(Error::ErrDataChannelClosed)?
            .label()
            .to_owned())
    }

    // Documented on `DataChannel::ordered`.
    async fn ordered(&self) -> Result<bool> {
        let mut peer_connection = self.inner.core.lock().await;
        Ok(peer_connection
            .data_channel(self.id)
            .ok_or(Error::ErrDataChannelClosed)?
            .ordered())
    }

    // Documented on `DataChannel::max_packet_life_time`.
    async fn max_packet_life_time(&self) -> Result<Option<u16>> {
        let mut peer_connection = self.inner.core.lock().await;
        Ok(peer_connection
            .data_channel(self.id)
            .ok_or(Error::ErrDataChannelClosed)?
            .max_packet_life_time())
    }

    // Documented on `DataChannel::max_retransmits`.
    async fn max_retransmits(&self) -> Result<Option<u16>> {
        let mut peer_connection = self.inner.core.lock().await;
        Ok(peer_connection
            .data_channel(self.id)
            .ok_or(Error::ErrDataChannelClosed)?
            .max_retransmits())
    }

    // Documented on `DataChannel::protocol`.
    async fn protocol(&self) -> Result<String> {
        let mut peer_connection = self.inner.core.lock().await;
        Ok(peer_connection
            .data_channel(self.id)
            .ok_or(Error::ErrDataChannelClosed)?
            .protocol()
            .to_owned())
    }

    // Documented on `DataChannel::negotiated`.
    async fn negotiated(&self) -> Result<bool> {
        let mut peer_connection = self.inner.core.lock().await;
        Ok(peer_connection
            .data_channel(self.id)
            .ok_or(Error::ErrDataChannelClosed)?
            .negotiated())
    }

    // Documented on `DataChannel::id`.
    fn id(&self) -> RTCDataChannelId {
        self.id
    }

    // Documented on `DataChannel::ready_state`.
    async fn ready_state(&self) -> Result<RTCDataChannelState> {
        let mut peer_connection = self.inner.core.lock().await;
        Ok(peer_connection
            .data_channel(self.id)
            .ok_or(Error::ErrDataChannelClosed)?
            .ready_state())
    }

    // Documented on `DataChannel::buffered_amount_high_threshold`.
    async fn buffered_amount_high_threshold(&self) -> Result<u32> {
        let mut peer_connection = self.inner.core.lock().await;
        Ok(peer_connection
            .data_channel(self.id)
            .ok_or(Error::ErrDataChannelClosed)?
            .buffered_amount_high_threshold())
    }

    // Documented on `DataChannel::set_buffered_amount_high_threshold`.
    async fn set_buffered_amount_high_threshold(&self, threshold: u32) -> Result<()> {
        {
            let mut peer_connection = self.inner.core.lock().await;
            peer_connection
                .data_channel(self.id)
                .ok_or(Error::ErrDataChannelClosed)?
                .set_buffered_amount_high_threshold(threshold);
        }

        self.inner.wake_writes().await;
        Ok(())
    }

    // Documented on `DataChannel::buffered_amount_low_threshold`.
    async fn buffered_amount_low_threshold(&self) -> Result<u32> {
        let mut peer_connection = self.inner.core.lock().await;
        Ok(peer_connection
            .data_channel(self.id)
            .ok_or(Error::ErrDataChannelClosed)?
            .buffered_amount_low_threshold())
    }

    async fn outstanding_bytes(&self) -> Result<usize> {
        let mut peer_connection = self.inner.core.lock().await;
        Ok(peer_connection
            .data_channel(self.id)
            .ok_or(Error::ErrDataChannelClosed)?
            .outstanding_bytes())
    }

    // Documented on `DataChannel::set_buffered_amount_low_threshold`.
    async fn set_buffered_amount_low_threshold(&self, threshold: u32) -> Result<()> {
        {
            let mut peer_connection = self.inner.core.lock().await;
            peer_connection
                .data_channel(self.id)
                .ok_or(Error::ErrDataChannelClosed)?
                .set_buffered_amount_low_threshold(threshold);
        }

        self.inner.wake_writes().await;
        Ok(())
    }

    // Contract (blocking behaviour, errors, example) is documented on `DataChannel::send`.
    async fn send(&self, data: BytesMut) -> Result<()> {
        self.wait_for_send_buffer().await?;
        {
            let mut peer_connection = self.inner.core.lock().await;
            if self.inner.closing.load(Ordering::Acquire) {
                return Err(Error::ErrDataChannelClosed);
            }
            let mut dc = peer_connection
                .data_channel(self.id)
                .ok_or(Error::ErrDataChannelClosed)?;
            dc.send(self.inner.runtime.now(), data)?;
        }

        // Wake the driver so it flushes SCTP output (poll_write) and checks
        // for newly generated events (e.g. OnBufferedAmountHigh).
        self.inner.wake_writes().await;
        Ok(())
    }

    // Contract is documented on `DataChannel::send_text`.
    async fn send_text(&self, text: &str) -> Result<()> {
        self.wait_for_send_buffer().await?;
        {
            let mut peer_connection = self.inner.core.lock().await;
            if self.inner.closing.load(Ordering::Acquire) {
                return Err(Error::ErrDataChannelClosed);
            }
            let mut dc = peer_connection
                .data_channel(self.id)
                .ok_or(Error::ErrDataChannelClosed)?;
            dc.send_text(self.inner.runtime.now(), text)?;
        }

        self.inner.wake_writes().await;
        Ok(())
    }

    async fn writable(&self) -> Result<()> {
        let limit = self.inner.data_channel_send_buffer_limit;
        let mut waiting = false;
        loop {
            let listener = waiting.then(|| self.inner.data_channel_backpressure.listen());
            if self.inner.closing.load(Ordering::Acquire) {
                return Err(Error::ErrDataChannelClosed);
            }
            {
                let mut peer_connection = self.inner.core.lock().await;
                if self.inner.closing.load(Ordering::Acquire) {
                    return Err(Error::ErrDataChannelClosed);
                }
                let dc = peer_connection
                    .data_channel(self.id)
                    .ok_or(Error::ErrDataChannelClosed)?;
                if matches!(
                    dc.ready_state(),
                    RTCDataChannelState::Closing | RTCDataChannelState::Closed
                ) {
                    return Err(Error::ErrDataChannelClosed);
                }
                if limit == usize::MAX || dc.outstanding_bytes() < limit {
                    return Ok(());
                }
            }
            if let Some(notified) = listener {
                futures::select! {
                    _ = notified.fuse() => {}
                    _ = self.inner.runtime.sleep(Duration::from_millis(50)).fuse() => {}
                }
            } else {
                // Register before checking again: closure or released capacity
                // can race the core lock above. Healthy sends allocate no listener.
                waiting = true;
            }
        }
    }

    async fn try_send(&self, data: BytesMut) -> Result<()> {
        // Non-blocking peer of `send`: same terminal-on-close guard, but instead of awaiting
        // capacity it fails fast. Back-pressure is folded into the send's own core-lock so
        // the fast path takes the lock exactly ONCE: peek outstanding_bytes and enqueue
        // atomically.
        if self.inner.closing.load(Ordering::Acquire) {
            return Err(Error::ErrDataChannelClosed);
        }
        let limit = self.inner.data_channel_send_buffer_limit;
        {
            let mut peer_connection = self.inner.core.lock().await;
            if self.inner.closing.load(Ordering::Acquire) {
                return Err(Error::ErrDataChannelClosed);
            }
            let mut dc = peer_connection
                .data_channel(self.id)
                .ok_or(Error::ErrDataChannelClosed)?;
            if matches!(
                dc.ready_state(),
                RTCDataChannelState::Closing | RTCDataChannelState::Closed
            ) {
                return Err(Error::ErrDataChannelClosed);
            }
            let outstanding = dc.outstanding_bytes();
            // Reject once the message would push outstanding bytes past the limit — but
            // always admit onto an empty buffer, so a lone message larger than the limit can
            // still be sent. saturating_add so an unbounded (`usize::MAX`) limit can't
            // overflow.
            if outstanding != 0 && outstanding.saturating_add(data.len().max(1)) > limit {
                return Err(Error::ErrSendBufferFull);
            }
            dc.send(self.inner.runtime.now(), data)?;
        }

        self.inner.wake_writes().await;
        Ok(())
    }

    async fn try_send_text(&self, text: &str) -> Result<()> {
        // Non-blocking peer of `send_text`; see `try_send`.
        if self.inner.closing.load(Ordering::Acquire) {
            return Err(Error::ErrDataChannelClosed);
        }
        let limit = self.inner.data_channel_send_buffer_limit;
        {
            let mut peer_connection = self.inner.core.lock().await;
            if self.inner.closing.load(Ordering::Acquire) {
                return Err(Error::ErrDataChannelClosed);
            }
            let mut dc = peer_connection
                .data_channel(self.id)
                .ok_or(Error::ErrDataChannelClosed)?;
            if matches!(
                dc.ready_state(),
                RTCDataChannelState::Closing | RTCDataChannelState::Closed
            ) {
                return Err(Error::ErrDataChannelClosed);
            }
            let outstanding = dc.outstanding_bytes();
            if outstanding != 0 && outstanding.saturating_add(text.len().max(1)) > limit {
                return Err(Error::ErrSendBufferFull);
            }
            dc.send_text(self.inner.runtime.now(), text)?;
        }

        self.inner.wake_writes().await;
        Ok(())
    }

    async fn poll(&self) -> Option<DataChannelEvent> {
        let event = self.evt_rx.lock().await.recv().await;
        // Taking an event frees a slot. If the driver is holding something it could not
        // deliver, say so now — it has no timer to fall back on.
        //
        // `Acquire`, pairing with the driver's `Release` store. Observing a stale `false`
        // means skipping the notify, and the driver then waits on the next inbound packet
        // instead — which under back-pressure is a throttled peer's zero-window probe, i.e.
        // a long time. Cheap enough not to trade for that: one load per received message,
        // and only reached when a message actually arrived.
        if event.is_some()
            && self
                .inner
                .data_channel_delivery_blocked
                .load(Ordering::Acquire)
        {
            self.inner.data_channel_consumed.notify_waiters();
        }
        event
    }

    async fn close(&self) -> Result<()> {
        {
            let mut peer_connection = self.inner.core.lock().await;
            let Some(mut channel) = peer_connection.data_channel(self.id) else {
                return Ok(());
            };
            channel.close()?;
        }

        self.inner.data_channel_backpressure.notify_waiters();
        self.inner.wake_writes().await;
        Ok(())
    }
}

// A built-in provider is required: these construct real peer connections, and construction
// resolves a provider. The no-built-in configuration is exercised by the provider tests in
// `tests/`, which supply their own.
#[cfg(all(test, any(feature = "crypto-ring", feature = "crypto-aws-lc-rs")))]
mod tests {
    use super::*;
    use futures::executor::block_on;
    use std::sync::atomic::AtomicUsize;

    // A once-polled writer can consume a mutex wake before it reaches capacity.
    // Keep those writers scheduled while another operation needs the same core.
    #[test]
    fn concurrent_close_drives_writers_queued_on_the_core_lock() {
        struct WakeCount(AtomicUsize);
        impl futures::task::ArcWake for WakeCount {
            fn wake_by_ref(this: &Arc<Self>) {
                this.0.fetch_add(1, Ordering::SeqCst);
            }
        }
        let runtime = crate::runtime::default_runtime().unwrap();
        runtime.block_on(Box::pin(async {
            let (mut inner, _events) = crate::peer_connection::new_test_peer_connection().await;
            Arc::get_mut(&mut inner)
                .unwrap()
                .data_channel_send_buffer_limit = 0;
            let id = inner
                .core
                .lock()
                .await
                .create_data_channel("pending", None)
                .unwrap()
                .id();
            let (_sender, receiver) = crate::runtime::channel(1);
            let dc = DataChannelImpl::new(id, inner.clone(), receiver);
            let core = inner.core.lock().await;
            let binary = dc.send(BytesMut::from(&b"x"[..]));
            let text = dc.send_text("x");
            let writable = dc.writable();
            let close = dc.close();
            futures::pin_mut!(binary, text, writable, close);
            for pending in [
                binary.as_mut().now_or_never(),
                text.as_mut().now_or_never(),
                writable.as_mut().now_or_never(),
            ] {
                assert!(pending.is_none(), "each writer waits on the held core");
            }
            let wakes = Arc::new(WakeCount(AtomicUsize::new(0)));
            let waker = futures::task::waker(wakes.clone());
            assert!(
                close
                    .as_mut()
                    .poll(&mut std::task::Context::from_waker(&waker))
                    .is_pending()
            );
            drop(core);
            assert_eq!(
                wakes.0.load(Ordering::SeqCst),
                0,
                "the mutex wakes the dormant first writer, not the later close"
            );
            assert!(
                inner.core.try_lock().is_some(),
                "the core itself is unlocked"
            );
            let (binary, text, writable, closed) = futures::join!(binary, text, writable, close);
            assert!(closed.is_ok());
            for result in [binary, text, writable] {
                assert!(matches!(result, Err(Error::ErrDataChannelClosed)));
            }
        }));
    }

    // Issue #1135: a writer can wait on the core lock when the driver stops.
    #[test]
    fn writable_observes_stop_before_capacity_wait_registration() {
        let runtime = crate::runtime::default_runtime().unwrap();
        runtime.block_on(Box::pin(async {
            for limit in [0, usize::MAX] {
            let (mut inner, _events) = crate::peer_connection::new_test_peer_connection().await;
            Arc::get_mut(&mut inner)
                .unwrap()
                .data_channel_send_buffer_limit = limit;
            let id = inner
                .core
                .lock()
                .await
                .create_data_channel("pending", None)
                .unwrap()
                .id();
            let (_sender, receiver) = crate::runtime::channel(1);
            let dc = DataChannelImpl::new(id, inner.clone(), receiver);
            let core = inner.core.lock().await;
            let writable = dc.writable();
            futures::pin_mut!(writable);
            assert!(
                writable.as_mut().now_or_never().is_none(),
                "writer waits for the held core lock"
            );
            inner.closing.store(true, Ordering::Release);
            inner.data_channel_backpressure.notify_waiters();
            drop(core);
            assert!(
                matches!(
                    writable.as_mut().now_or_never(),
                    Some(Err(Error::ErrDataChannelClosed))
                ),
                "a terminal wake before capacity registration must not require the polling timer"
            );
            }
        }));
    }

    // Issue #1135: stop can overtake every send while it acquires the core.
    #[test]
    fn sends_observe_stop_before_core_enqueue() {
        let runtime = crate::runtime::default_runtime().unwrap();
        runtime.block_on(Box::pin(async {
            let (inner, _events) = crate::peer_connection::new_test_peer_connection().await;
            let id = inner
                .core
                .lock()
                .await
                .create_data_channel("pending", None)
                .unwrap()
                .id();
            let (_sender, receiver) = crate::runtime::channel(1);
            let dc = DataChannelImpl::new(id, inner.clone(), receiver);
            let core = inner.core.lock().await;
            let mut writes = vec![
                dc.send(BytesMut::from(&b"x"[..])).boxed(),
                dc.send_text("x").boxed(),
                dc.try_send(BytesMut::from(&b"x"[..])).boxed(),
                dc.try_send_text("x").boxed(),
            ];
            for write in &mut writes {
                assert!(write.as_mut().now_or_never().is_none());
            }
            inner.closing.store(true, Ordering::Release);
            inner.data_channel_backpressure.notify_waiters();
            drop(core);
            for write in &mut writes {
                assert!(
                    matches!(
                        write.as_mut().now_or_never(),
                        Some(Err(Error::ErrDataChannelClosed))
                    ),
                    "a stopped send must not enter the live channel path"
                );
            }
        }));
    }

    /// A minimal external `DataChannel` that overrides only `send`/`send_text` (to record that
    /// they ran) and leaves `outstanding_bytes`, `writable`, `try_send` and `try_send_text` to
    /// the trait's **default bodies**.
    ///
    /// This locks in the backward-compatibility contract the send back-pressure work relies on:
    /// a downstream `impl DataChannel` that predates these methods must keep compiling and get
    /// sensible defaults — `outstanding_bytes` reports `0` ("no back-pressure signal"),
    /// `writable` resolves `Ok(())`, and `try_send`/`try_send_text` delegate to `send`/`send_text`
    /// (correct where `send` is already non-blocking). Every other method is unreachable here and
    /// left `unimplemented!()`.
    #[derive(Default)]
    struct DefaultDataChannel {
        sends: AtomicUsize,
        text_sends: AtomicUsize,
    }

    impl crate::sealed::Sealed for DefaultDataChannel {}

    #[async_trait::async_trait]
    impl DataChannel for DefaultDataChannel {
        async fn label(&self) -> Result<String> {
            unimplemented!()
        }
        async fn ordered(&self) -> Result<bool> {
            unimplemented!()
        }
        async fn max_packet_life_time(&self) -> Result<Option<u16>> {
            unimplemented!()
        }
        async fn max_retransmits(&self) -> Result<Option<u16>> {
            unimplemented!()
        }
        async fn protocol(&self) -> Result<String> {
            unimplemented!()
        }
        async fn negotiated(&self) -> Result<bool> {
            unimplemented!()
        }
        fn id(&self) -> RTCDataChannelId {
            unimplemented!()
        }
        async fn ready_state(&self) -> Result<RTCDataChannelState> {
            unimplemented!()
        }
        async fn buffered_amount_high_threshold(&self) -> Result<u32> {
            unimplemented!()
        }
        async fn set_buffered_amount_high_threshold(&self, _threshold: u32) -> Result<()> {
            unimplemented!()
        }
        async fn buffered_amount_low_threshold(&self) -> Result<u32> {
            unimplemented!()
        }
        async fn set_buffered_amount_low_threshold(&self, _threshold: u32) -> Result<()> {
            unimplemented!()
        }
        // outstanding_bytes(), writable(), try_send(), try_send_text(): deliberately NOT
        // overridden — exercising the trait defaults is the whole point of this test.
        async fn send(&self, _data: BytesMut) -> Result<()> {
            self.sends.fetch_add(1, Ordering::Relaxed);
            Ok(())
        }
        async fn send_text(&self, _text: &str) -> Result<()> {
            self.text_sends.fetch_add(1, Ordering::Relaxed);
            Ok(())
        }
        async fn poll(&self) -> Option<DataChannelEvent> {
            unimplemented!()
        }
        async fn close(&self) -> Result<()> {
            unimplemented!()
        }
    }

    #[test]
    fn outstanding_bytes_trait_default_is_zero() {
        let dc = DefaultDataChannel::default();
        let n =
            block_on(dc.outstanding_bytes()).expect("default outstanding_bytes() must return Ok");
        assert_eq!(
            n, 0,
            "the DataChannel::outstanding_bytes default must report 0 outstanding bytes"
        );
    }

    #[test]
    fn writable_trait_default_is_ok() {
        let dc = DefaultDataChannel::default();
        block_on(dc.writable()).expect("the DataChannel::writable default must resolve Ok(())");
    }

    #[test]
    fn try_send_trait_default_delegates_to_send() {
        let dc = DefaultDataChannel::default();
        block_on(dc.try_send(BytesMut::new())).expect("default try_send() must return Ok");
        assert_eq!(
            dc.sends.load(Ordering::Relaxed),
            1,
            "the DataChannel::try_send default must delegate to send()"
        );
        assert_eq!(dc.text_sends.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn try_send_text_trait_default_delegates_to_send_text() {
        let dc = DefaultDataChannel::default();
        block_on(dc.try_send_text("x")).expect("default try_send_text() must return Ok");
        assert_eq!(
            dc.text_sends.load(Ordering::Relaxed),
            1,
            "the DataChannel::try_send_text default must delegate to send_text()"
        );
        assert_eq!(dc.sends.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn drop_removes_event_sender() {
        use crate::peer_connection::new_test_peer_connection;
        use crate::runtime::{channel, default_runtime};

        let rt = default_runtime().expect("test requires a runtime feature");
        rt.block_on(Box::pin(async {
            let (inner, _driver_event_rx) = new_test_peer_connection().await;

            let channel_id = 0;
            let (evt_tx, evt_rx) = channel::<DataChannelEvent>(1);
            inner
                .data_channel_events_tx
                .lock()
                .await
                .insert(channel_id, evt_tx);
            assert!(
                inner
                    .data_channel_events_tx
                    .lock()
                    .await
                    .contains_key(&channel_id)
            );

            let dc = DataChannelImpl::new(channel_id, inner.clone(), evt_rx);
            drop(dc);

            assert!(
                !inner
                    .data_channel_events_tx
                    .lock()
                    .await
                    .contains_key(&channel_id)
            );
        }));
    }
}
