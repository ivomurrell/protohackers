use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};
use tracing::{info, instrument, Instrument};

#[instrument]
pub async fn run() {
    let listener = TcpListener::bind("0.0.0.0:10000")
        .await
        .expect("failed to bind socket");
    loop {
        let (mut socket, _) = listener
            .accept()
            .await
            .expect("failed to accept connection");
        let addr = socket.peer_addr().expect("failed to get peer address");
        tokio::spawn(
            async move {
                info!("accepted connection");
                let mut buf = Vec::new();
                socket
                    .read_to_end(&mut buf)
                    .await
                    .expect("failed to read from socket");
                socket
                    .write_all(&buf)
                    .await
                    .expect("failed to write to socket");
            }
            .instrument(tracing::info_span!("handler", ?addr)),
        );
    }
}
