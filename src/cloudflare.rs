use crate::config::ClientConfig;
use crate::protocol::{ClientWsMessage, ServerWsMessage};
use anyhow::{Result, anyhow};
use futures_util::{SinkExt, StreamExt};
use reqwest::Client;
use tokio::sync::mpsc;
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::header::AUTHORIZATION;

pub struct CloudflareRelay {
    config: ClientConfig,
    http: Client,
}

impl CloudflareRelay {
    pub fn new(config: ClientConfig) -> Self {
        Self {
            config,
            http: Client::new(),
        }
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

        tokio::spawn(async move {
            while let Some(message) = out_rx.recv().await {
                match serde_json::to_string(&message) {
                    Ok(json) => {
                        if write.send(Message::Text(json.into())).await.is_err() {
                            break;
                        }
                    }
                    Err(err) => log::warn!("failed to encode websocket message: {:?}", err),
                }
            }
        });

        tokio::spawn(async move {
            while let Some(message) = read.next().await {
                match message {
                    Ok(Message::Text(text)) => {
                        match serde_json::from_str::<ServerWsMessage>(&text) {
                            Ok(decoded) => {
                                if in_tx.send(decoded).await.is_err() {
                                    break;
                                }
                            }
                            Err(err) => log::warn!("failed to decode websocket message: {:?}", err),
                        }
                    }
                    Ok(Message::Close(_)) => break,
                    Ok(_) => {}
                    Err(err) => {
                        log::warn!("websocket read failed: {:?}", err);
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
