//! The requests the monitor makes: one client, and retries where a retry can help.

use std::{io::Read, thread, time::Duration};

use anyhow::{Result, bail};
use reqwest::blocking::{Client, RequestBuilder};

const USER_AGENT: &str = "surfpool-scenarios-monitoring";
/// An IDL is served in a second or two; a request that takes this long is not going to answer.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const ATTEMPTS: u32 = 3;
/// Upstream IDLs are a few MB at most; anything far larger is not an IDL.
const MAX_RESPONSE_BYTES: u64 = 32 * 1024 * 1024;

pub fn client() -> Result<Client> {
    let client = Client::builder()
        .user_agent(USER_AGENT)
        .timeout(REQUEST_TIMEOUT)
        .build()?;
    Ok(client)
}

/// Sends with retries on rate limiting, server errors and transport failures, including one
/// while reading the body. A 4xx other than 429 is returned as is: it will not change on retry.
pub fn send(request: RequestBuilder) -> Result<(u16, Vec<u8>)> {
    let mut last = String::from("request failed");
    for attempt in 0..ATTEMPTS {
        if attempt > 0 {
            thread::sleep(Duration::from_secs(1 << attempt));
        }
        let request = request
            .try_clone()
            .expect("requests without a streaming body clone");
        let response = match request.send() {
            Ok(response) => response,
            Err(error) => {
                last = error.without_url().to_string();
                continue;
            }
        };
        let status = response.status().as_u16();
        if status == 429 || status >= 500 {
            last = format!("HTTP {status}");
            continue;
        }
        let mut bytes = Vec::new();
        match response
            .take(MAX_RESPONSE_BYTES + 1)
            .read_to_end(&mut bytes)
        {
            Ok(_) if bytes.len() as u64 > MAX_RESPONSE_BYTES => {
                bail!("the response exceeds {MAX_RESPONSE_BYTES} bytes")
            }
            Ok(_) => return Ok((status, bytes)),
            Err(error) => last = format!("reading the response: {error}"),
        }
    }
    bail!("{last}")
}
