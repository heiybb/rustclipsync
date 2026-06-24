use crate::config::ClientConfig;
use crate::protocol::{ClientWsMessage, ServerWsMessage};
use anyhow::{Result, anyhow};
use futures_util::{SinkExt, StreamExt};
use reqwest::Client;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::header::AUTHORIZATION;

/// How often we ping the relay to keep the connection warm.
const PING_INTERVAL: Duration = Duration::from_secs(20);
/// If we hear nothing back (data or pong) for this long, the connection is
/// treated as dead even when the OS never surfaces an error. This catches the
/// half-open case where Cloudflare recycles the relay/Durable Object without a
/// clean reset: `read.next()` would otherwise block forever and the reconnect
/// loop would never fire. Must comfortably exceed `PING_INTERVAL` so a healthy
/// connection (which sees a pong every cycle) never trips it.
const READ_IDLE_TIMEOUT: Duration = Duration::from_secs(60);

pub struct CloudflareRelay {
    config: ClientConfig,
    http: Client,
}

impl CloudflareRelay {
    pub fn new(config: ClientConfig) -> Self {
        let http = Client::builder()
            .connect_timeout(Duration::from_secs(10))
            // Per-read timeout, not a total timeout: a stalled transfer errors
            // out instead of hanging the receive loop forever, while a large
            // but still-progressing object transfer is left alone.
            .read_timeout(Duration::from_secs(30))
            .build()
            .unwrap_or_default();
        Self { config, http }
    }

    pub async fn connect(
        &self,
        last_seen_sequence: u64,
    ) -> Result<(
        mpsc::Sender<ClientWsMessage>,
        mpsc::Receiver<ServerWsMessage>,
    )> {
        let mut request = ws_endpoint(&self.config.server_url)?.into_client_request()?;
        request.headers_mut().insert(
            AUTHORIZATION,
            format!("Bearer {}", self.config.auth_token).parse()?,
        );
        let (socket, _) = connect_async(request).await?;
        let (mut write, mut read) = socket.split();
        let (out_tx, mut out_rx) = mpsc::channel::<ClientWsMessage>(64);
        let (in_tx, in_rx) = mpsc::channel::<ServerWsMessage>(64);

        let hello = ClientWsMessage::Hello {
            client_id: self.config.client_id.clone(),
            client_name: self.config.client_name.clone(),
            last_seen_sequence,
        };
        write
            .send(Message::Text(serde_json::to_string(&hello)?.into()))
            .await?;

        let (close_tx, mut close_rx) = tokio::sync::oneshot::channel::<()>();

        tokio::spawn(async move {
            let mut ping_interval = tokio::time::interval(PING_INTERVAL);
            ping_interval.tick().await;

            loop {
                tokio::select! {
                    message = out_rx.recv() => {
                        match message {
                            Some(message) => {
                                match serde_json::to_string(&message) {
                                    Ok(json) => {
                                        if write.send(Message::Text(json.into())).await.is_err() {
                                            break;
                                        }
                                    }
                                    Err(err) => log::warn!("failed to encode websocket message: {:?}", err),
                                }
                            }
                            None => break,
                        }
                    }
                    _ = ping_interval.tick() => {
                        if write.send(Message::Ping(bytes::Bytes::new())).await.is_err() {
                            break;
                        }
                    }
                }
            }
            let _ = close_tx.send(());
        });

        tokio::spawn(async move {
            loop {
                tokio::select! {
                    message = tokio::time::timeout(READ_IDLE_TIMEOUT, read.next()) => {
                        match message {
                            Err(_elapsed) => {
                                // No data or pong within the idle window: the
                                // connection is half-open (e.g. relay recycled
                                // without a clean reset). Drop it so the
                                // reconnect loop can establish a fresh socket.
                                log::info!("websocket idle timeout, treating connection as dead");
                                break;
                            }
                            Ok(Some(Ok(Message::Text(text)))) => {
                                match serde_json::from_str::<ServerWsMessage>(&text) {
                                    Ok(decoded) => {
                                        if in_tx.send(decoded).await.is_err() {
                                            break;
                                        }
                                    }
                                    Err(err) => log::warn!("failed to decode websocket message: {:?}", err),
                                }
                            }
                            Ok(Some(Ok(Message::Close(_)))) => break,
                            Ok(Some(Ok(_))) => {}
                            Ok(Some(Err(err))) => {
                                match &err {
                                    tokio_tungstenite::tungstenite::Error::ConnectionClosed => {
                                        log::info!("websocket connection closed by remote peer");
                                    }
                                    tokio_tungstenite::tungstenite::Error::Protocol(
                                        tokio_tungstenite::tungstenite::error::ProtocolError::ResetWithoutClosingHandshake,
                                    ) => {
                                        log::info!("websocket connection reset by remote peer without closing handshake");
                                    }
                                    tokio_tungstenite::tungstenite::Error::Io(io_err)
                                        if io_err.kind() == std::io::ErrorKind::ConnectionReset
                                            || io_err.kind() == std::io::ErrorKind::ConnectionAborted
                                            || io_err.kind() == std::io::ErrorKind::BrokenPipe
                                            || io_err.kind() == std::io::ErrorKind::UnexpectedEof =>
                                    {
                                        log::info!("websocket connection dropped: {:?}", io_err);
                                    }
                                    _ => {
                                        log::warn!("websocket read failed: {:?}", err);
                                    }
                                }
                                break;
                            }
                            Ok(None) => break,
                        }
                    }
                    _ = &mut close_rx => {
                        break;
                    }
                }
            }
        });

        Ok((out_tx, in_rx))
    }

    pub async fn upload_object(
        &self,
        message_id: &str,
        filename: Option<&str>,
        bytes: Vec<u8>,
    ) -> Result<String> {
        let url = object_endpoint(&self.config.server_url, message_id, filename)?;
        let total_size = bytes.len() as u64;
        let bytes_data = bytes::Bytes::from(bytes);

        use indicatif::{ProgressBar, ProgressStyle};
        let pb = ProgressBar::new(total_size);
        pb.set_style(
            ProgressStyle::default_bar()
                .template("{spinner:.green} Uploading [{elapsed_precise}] [{bar:40.magenta/blue}] {bytes}/{total_bytes} ({eta})")?
                .progress_chars("#>-"),
        );

        let pb_inner = pb.clone();
        let mut position = 0;
        let stream = futures_util::stream::poll_fn(move |_| {
            if position >= bytes_data.len() {
                return std::task::Poll::Ready(None);
            }
            let end = std::cmp::min(position + 64 * 1024, bytes_data.len());
            let chunk = bytes_data.slice(position..end);
            position = end;
            pb_inner.inc(chunk.len() as u64);
            std::task::Poll::Ready(Some(Ok::<_, std::io::Error>(chunk)))
        });

        let response = self
            .http
            .put(url)
            .bearer_auth(&self.config.auth_token)
            .body(reqwest::Body::wrap_stream(stream))
            .send()
            .await?
            .error_for_status()?
            .json::<serde_json::Value>()
            .await?;

        pb.set_style(
            ProgressStyle::default_bar()
                .template("{spinner:.green} Uploaded [{elapsed_precise}] [{bar:40.magenta/blue}] {bytes}/{total_bytes} (finished)")?
                .progress_chars("#>-"),
        );
        pb.finish();
        println!();

        response
            .get("object_key")
            .and_then(|value| value.as_str())
            .map(str::to_string)
            .ok_or_else(|| anyhow!("missing object_key in upload response"))
    }

    pub async fn download_object(
        &self,
        message_id: &str,
        filename: Option<&str>,
    ) -> Result<Vec<u8>> {
        let url = object_endpoint(&self.config.server_url, message_id, filename)?;
        let response = self
            .http
            .get(url)
            .bearer_auth(&self.config.auth_token)
            .send()
            .await?
            .error_for_status()?;

        let total_size = response
            .content_length()
            .ok_or_else(|| anyhow!("failed to get content-length from response"))?;

        let mut downloaded: u64 = 0;
        let mut buffer = Vec::with_capacity(total_size as usize);
        let mut stream = response.bytes_stream();

        let _display_name = filename.unwrap_or("unnamed file");
        use indicatif::{ProgressBar, ProgressStyle};

        let pb = ProgressBar::new(total_size);
        pb.set_style(
            ProgressStyle::default_bar()
                .template("{spinner:.green} [{elapsed_precise}] [{bar:40.cyan/blue}] {bytes}/{total_bytes} ({eta})")?
                .progress_chars("#>-"),
        );

        while let Some(item) = stream.next().await {
            let chunk = item?;
            buffer.extend_from_slice(&chunk);
            downloaded += chunk.len() as u64;
            pb.set_position(downloaded);
        }
        pb.set_style(
            ProgressStyle::default_bar()
                .template("{spinner:.green} Downloaded [{elapsed_precise}] [{bar:40.cyan/blue}] {bytes}/{total_bytes} (finished)")?
                .progress_chars("#>-"),
            );
        pb.finish();
        println!();

        Ok(buffer)
    }
}

fn ws_endpoint(server_url: &str) -> Result<String> {
    let trimmed = server_url.trim_end_matches('/');
    if let Some(rest) = trimmed.strip_prefix("https://") {
        Ok(format!("wss://{rest}/ws"))
    } else if let Some(rest) = trimmed.strip_prefix("http://") {
        Ok(format!("ws://{rest}/ws"))
    } else {
        Err(anyhow!("server url must start with http:// or https://"))
    }
}

fn object_endpoint(server_url: &str, message_id: &str, filename: Option<&str>) -> Result<String> {
    let mut url = format!(
        "{}/objects/{}",
        server_url.trim_end_matches('/'),
        urlencoding::encode(message_id)
    );
    if let Some(filename) = filename {
        url.push_str("?filename=");
        url.push_str(&urlencoding::encode(filename));
    }
    Ok(url)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_ws_endpoint_from_https_server_url() {
        assert_eq!(
            ws_endpoint("https://clipsync.example.com").unwrap(),
            "wss://clipsync.example.com/ws"
        );
    }

    #[test]
    fn builds_object_endpoint_with_filename() {
        assert_eq!(
            object_endpoint("https://clipsync.example.com/", "m1", Some("sample.txt")).unwrap(),
            "https://clipsync.example.com/objects/m1?filename=sample.txt"
        );
    }
}
