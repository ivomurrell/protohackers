use std::{
    collections::HashSet,
    sync::{Arc, Mutex},
};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::TcpListener,
    sync::broadcast,
};
use tracing::{info, instrument, warn, Instrument};

type UserList = Arc<Mutex<HashSet<String>>>;

#[instrument]
pub async fn run() {
    let (tx, _) = broadcast::channel(16);
    let user_list: UserList = Arc::new(Mutex::new(HashSet::new()));

    let listener = TcpListener::bind("0.0.0.0:10003")
        .await
        .expect("failed to bind socket");
    loop {
        let (mut socket, addr) = listener
            .accept()
            .await
            .expect("failed to accept connection");

        let tx = tx.clone();
        let user_list = Arc::clone(&user_list);

        tokio::spawn(
            async move {
                info!("accepted connection");
                let (reader, mut writer) = socket.split();
                let mut line_reader = BufReader::new(reader).lines();

                writer
                    .write_all("hey, what's your name?\n".as_bytes())
                    .await
                    .expect("failed to write initial message");
                let username = line_reader
                    .next_line()
                    .await
                    .expect("failed to read username")
                    .expect("socket closed before username set");
                if username.len() > 32 {
                    warn!(username, "username was too long");
                    return;
                }
                let users: Vec<String>;
                {
                    let mut user_list = user_list.lock().expect("user list was poisoned");
                    if user_list.contains(&username) {
                        warn!(username, "user already exists");
                        return;
                    }
                    users = user_list.iter().cloned().collect();
                    user_list.insert(username.clone());
                }
                async {
                    let presence_notif = format!(
                        "* this room currently has {} in it\n",
                        if users.is_empty() {
                            "nobody".to_owned()
                        } else {
                            users.join(", ")
                        }
                    );
                    if let Err(err) = writer.write_all(presence_notif.as_bytes()).await {
                        warn!(username, error = ?err, "couldn't write presence to socket");
                        return;
                    }
                    let join_message = format!("* {username} has joined the chat\n");
                    let _ = tx.send(join_message);

                    let mut rx = tx.subscribe();
                    loop {
                        tokio::select! {
                            Ok(message) = rx.recv() => {
                                if !message.starts_with(&format!("[{username}]")) {
                                    if let Err(err) = writer.write_all(message.as_bytes()).await {
                                        warn!(username, error = ?err, "couldn't write to socket");
                                        return;
                                    }
                                }
                            }
                            message = line_reader.next_line() => {
                                match message {
                                    Ok(Some(message)) => {
                                        let chat_message = format!("[{username}] {message}\n");
                                        let _ = tx.send(chat_message);
                                    }
                                    _ => {
                                        warn!(username, "socket has closed");
                                        return;
                                    }
                                }
                            }
                            else => {
                                warn!(username, "defaulting to ending connection");
                                return;
                            }
                        }
                    }
                }
                .await;

                let leaving_message = format!("* {username} has left the chat\n");
                let _ = tx.send(leaving_message);
                user_list
                    .lock()
                    .expect("user list was poisoned")
                    .remove(&username);
            }
            .instrument(tracing::info_span!("handler", ?addr)),
        );
    }
}
