//! Data channel event types.
//!
//! Events related to RTCDataChannel lifecycle and buffering state changes.

use crate::data_channel::RTCDataChannelId;

/// Events that can be emitted by an RTCDataChannel.
///
/// These events track the lifecycle and buffer state of data channels.
/// Poll `RTCPeerConnectionEvent::OnDataChannel` for errors and buffer changes.
/// Normal opening, input, and graceful closure share the ordered data-read path.
/// Async adapters map those read markers to the lifecycle variants below.
///
/// # Lifecycle Events
///
/// Data channels go through the following lifecycle:
/// 1. `OnOpen` - Channel is ready for data transmission
/// 2. `OnClosing` - Channel is starting to close
/// 3. `OnClose` - Channel is fully closed
/// 4. `OnError` - An error occurred
///
/// # Buffer Events
///
/// - `OnBufferedAmountLow` - Buffer dropped below low-water mark (safe to send more)
/// - `OnBufferedAmountHigh` - Buffer exceeded high-water mark (should pause sending)
///
/// # Examples
///
/// ## Handling channel lifecycle in order with input
///
/// Normal opening and graceful closure use the data-read path. The async
/// adapter converts these markers to `OnOpen` and `OnClose` callbacks.
///
/// ```
/// use rtc::peer_connection::message::RTCMessage;
///
/// # fn example(message: RTCMessage) {
/// match message {
///     RTCMessage::DataChannelOpened(id) => println!("Data channel opened: {id:?}"),
///     RTCMessage::DataChannelClosed(id) => println!("Data channel closed: {id:?}"),
///     RTCMessage::DataChannelMessage(id, data) => {
///         println!("Channel {id:?} received {} bytes", data.data.len());
///     }
///     _ => {}
/// }
/// # }
/// ```
///
/// ## Implementing flow control with buffer events
///
/// ```
/// use rtc::peer_connection::event::{RTCPeerConnectionEvent, RTCDataChannelEvent};
///
/// # fn example(event: RTCPeerConnectionEvent, paused: &mut bool) {
/// match event {
///     RTCPeerConnectionEvent::OnDataChannel(dc_event) => {
///         match dc_event {
///             RTCDataChannelEvent::OnBufferedAmountLow(channel_id) => {
///                 println!("Buffer low - resume sending");
///                 *paused = false;
///             }
///             RTCDataChannelEvent::OnBufferedAmountHigh(channel_id) => {
///                 println!("Buffer high - pause sending");
///                 *paused = true;
///             }
///             _ => {}
///         }
///     }
///     _ => {}
/// }
/// # }
/// ```
///
/// # See Also
///
/// - [W3C RTCDataChannel](https://www.w3.org/TR/webrtc/#rtcdatachannel)
#[allow(clippy::enum_variant_names)]
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum RTCDataChannelEvent {
    /// Data channel has opened and is ready to send/receive data.
    ///
    /// This event is fired when the data channel transitions to the "open" state.
    /// Data can now be sent through the channel.
    OnOpen(RTCDataChannelId),

    /// An error occurred on the data channel.
    ///
    /// This event is fired when an error is encountered. The channel may still
    /// be usable depending on the error type.
    OnError(RTCDataChannelId),

    /// Data channel is closing.
    ///
    /// This event is fired when the channel begins the closing process.
    /// The channel is transitioning to the "closing" state.
    OnClosing(RTCDataChannelId),

    /// Data channel has closed.
    ///
    /// This event is fired when the channel is fully closed and no longer usable.
    /// No more data can be sent or received.
    OnClose(RTCDataChannelId),

    /// Buffered amount dropped below the low-water mark.
    ///
    /// This event is fired when the amount of buffered outgoing data drops
    /// below the threshold set by `set_buffered_amount_low_threshold()`.
    /// This indicates it's safe to send more data without causing excessive buffering.
    ///
    /// Use this event to implement flow control and prevent memory exhaustion.
    OnBufferedAmountLow(RTCDataChannelId),

    /// Buffered amount exceeded the high-water mark (implementation-specific).
    ///
    /// This is a non-standard event that can be used to detect when too much
    /// data is being buffered. Applications should pause sending when this fires.
    OnBufferedAmountHigh(RTCDataChannelId),
}

impl Default for RTCDataChannelEvent {
    fn default() -> Self {
        Self::OnOpen(Default::default())
    }
}
