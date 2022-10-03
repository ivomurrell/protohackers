use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};

#[tokio::main]
async fn main() {
    let listener = TcpListener::bind("0.0.0.0:10000")
        .await
        .expect("failed to bind socket");
    loop {
        let (mut socket, _) = listener
            .accept()
            .await
            .expect("failed to accept connection");
        tokio::spawn(async move {
            let mut buf = Vec::new();
            socket
                .read_to_end(&mut buf)
                .await
                .expect("failed to read from socket");
            socket
                .write_all(&buf)
                .await
                .expect("failed to write to socket");
        });
    }
}
