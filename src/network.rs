use crate::config::ClientConfig;
use crate::protocol::{PullResponse, PushRequest, PushResponse};
use anyhow::Result;
use reqwest::Client;

pub struct HttpRelayClient {
    client: Client,
    server_url: String,
    auth_token: String,
}

impl HttpRelayClient {
    pub fn new(config: &ClientConfig) -> Self {
        Self {
            client: Client::new(),
            server_url: config.server_url.clone(),
            auth_token: config.auth_token.clone(),
        }
    }

    pub async fn push(&self, request: &PushRequest) -> Result<PushResponse> {
        let response = self
            .client
            .post(endpoint(&self.server_url, "/push"))
            .bearer_auth(&self.auth_token)
            .json(request)
            .send()
            .await?
            .error_for_status()?
            .json::<PushResponse>()
            .await?;
        Ok(response)
    }

    pub async fn pull(&self, client_id: &str, after: u64) -> Result<PullResponse> {
        let response = self
            .client
            .get(endpoint(&self.server_url, "/pull"))
            .bearer_auth(&self.auth_token)
            .query(&[
                ("client_id", client_id.to_string()),
                ("after", after.to_string()),
            ])
            .send()
            .await?
            .error_for_status()?
            .json::<PullResponse>()
            .await?;
        Ok(response)
    }
}

fn endpoint(server_url: &str, path: &str) -> String {
    format!("{}{}", server_url.trim_end_matches('/'), path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn joins_server_url_and_path() {
        assert_eq!(
            endpoint("http://127.0.0.1:7878", "/push"),
            "http://127.0.0.1:7878/push"
        );
        assert_eq!(
            endpoint("http://127.0.0.1:7878/", "/pull"),
            "http://127.0.0.1:7878/pull"
        );
    }
}
