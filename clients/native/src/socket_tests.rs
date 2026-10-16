use std::time::Duration;

use futures_util::SinkExt;
use tokio::net::{TcpSocket, TcpStream};
use tokio_tungstenite::{tungstenite::Message, MaybeTlsStream, WebSocketStream};

use crate::wire::WsStream;

pub(crate) async fn websocket_pair() -> (WsStream, WebSocketStream<TcpStream>) {
    let listener_socket = TcpSocket::new_v4().unwrap();
    listener_socket.set_recv_buffer_size(1024).unwrap();
    listener_socket
        .bind("127.0.0.1:0".parse().unwrap())
        .unwrap();
    let listener = listener_socket.listen(1).unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (tcp, _) = listener.accept().await.unwrap();
        let tcp = tcp.into_std().unwrap();
        let control = TcpSocket::from_std_stream(tcp.try_clone().unwrap());
        let inherited = control.recv_buffer_size().unwrap();
        control.set_recv_buffer_size(1024).unwrap();
        eprintln!(
            "backpressure receiver: inherited={inherited}, configured={}",
            control.recv_buffer_size().unwrap()
        );
        let tcp = TcpStream::from_std(tcp).unwrap();
        tokio_tungstenite::accept_async(tcp).await.unwrap()
    });
    let socket = TcpSocket::new_v4().unwrap();
    socket.set_send_buffer_size(1024).unwrap();
    eprintln!(
        "backpressure sender: {}",
        socket.send_buffer_size().unwrap()
    );
    let tcp = socket.connect(address).await.unwrap();
    let (client, _) =
        tokio_tungstenite::client_async(format!("ws://{address}"), MaybeTlsStream::Plain(tcp))
            .await
            .unwrap();
    (client, server.await.unwrap())
}

pub(crate) async fn queue_until_blocked(ws: &mut WsStream, frame: Message) -> usize {
    for count in 1..=16 {
        match tokio::time::timeout(Duration::from_millis(50), ws.send(frame.clone())).await {
            Err(_) => {
                eprintln!("backpressure reached after {count} fixture frames");
                return count;
            }
            Ok(result) => result.expect("a setup write must succeed or remain pending"),
        }
    }
    panic!("no socket backpressure after 16 fixture frames");
}
