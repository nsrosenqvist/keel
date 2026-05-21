//! Shared HTTP client for self-update requests.

use std::time::Duration;

pub fn build_client() -> reqwest::Result<reqwest::Client> {
    reqwest::Client::builder()
        .user_agent(concat!("croft/", env!("CARGO_PKG_VERSION")))
        .timeout(Duration::from_secs(60))
        .build()
}
