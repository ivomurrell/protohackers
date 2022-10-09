use std::{collections::HashMap, env, str, sync::Arc};
use tokio::{net::UdpSocket, sync::RwLock};
use tracing::{info, instrument, Instrument};

#[instrument]
pub async fn run() {
    let host = if env::var("FLY").is_ok() {
        "fly-global-services"
    } else {
        "0.0.0.0"
    };
    let listener = Arc::new(
        UdpSocket::bind((host, 10004))
            .await
            .expect("failed to bind socket"),
    );
    let db = Arc::new(RwLock::new(HashMap::from([(
        "version".to_owned(),
        "ivo's db 0.1".to_owned(),
    )])));

    loop {
        let mut buf = [0; 1024];
        let listener = Arc::clone(&listener);
        let (len, addr) = listener
            .recv_from(&mut buf)
            .await
            .expect("failed to receive message");
        let db = Arc::clone(&db);
        tokio::spawn(
            async move {
                let message = str::from_utf8(&buf[..len]).expect("message was not valid UTF-8");
                info!(?message, "received message");
                match message.split_once('=') {
                    Some((key, value)) => {
                        if key != "version" {
                            db.write().await.insert(key.to_owned(), value.to_owned());
                        }
                    }
                    None => {
                        let db = db.read().await;
                        let value = db.get(message).map_or("", |value| value.as_str());
                        let response = format!("{message}={value}");
                        listener
                            .send_to(response.as_bytes(), addr)
                            .await
                            .expect("failed to send response");
                    }
                };
            }
            .instrument(tracing::info_span!("handler", ?addr)),
        );
    }
}
