//! Reads the IDLs a program publishes from the Solana IDL service (https://idl.solana.com/docs).

use std::{io::Read, thread, time::Duration};

use anyhow::{Context, Result, bail};
use reqwest::blocking::{Client, RequestBuilder};
use serde_json::Value;

const USER_AGENT: &str = "surfpool-scenarios-monitoring";
const BASE_URL: &str = "https://idl.solana.com";
/// Upstream IDLs are a few MB at most; anything far larger is not an IDL.
const MAX_RESPONSE_BYTES: u64 = 32 * 1024 * 1024;

/// One published IDL: the text exactly as published, and the document it holds.
#[derive(Debug)]
pub struct Published {
    pub raw: String,
    pub document: Value,
}

/// The live IDL each source publishes for one program, `None` where it publishes none.
#[derive(Debug, Default)]
pub struct PublishedIdls {
    pub anchor: Option<Published>,
    pub pmp: Option<Published>,
}

impl PublishedIdls {
    pub fn iter(&self) -> impl Iterator<Item = (&'static str, Option<&Published>)> {
        [("anchor", self.anchor.as_ref()), ("pmp", self.pmp.as_ref())].into_iter()
    }
}

pub struct IdlService {
    client: Client,
}

impl IdlService {
    pub fn new() -> Result<Self> {
        let client = Client::builder()
            .user_agent(USER_AGENT)
            .timeout(Duration::from_secs(90))
            .build()?;
        Ok(Self { client })
    }

    /// The IDLs `program_id` publishes on mainnet right now.
    pub fn latest(&self, program_id: &str) -> Result<PublishedIdls> {
        let request = self
            .client
            .get(format!("{BASE_URL}/api/latest"))
            .query(&[("programId", program_id), ("cluster", "mainnet-beta")]);
        let (status, bytes) = send(request, 3)?;
        if status != 200 {
            bail!("IDL service: HTTP {status}");
        }
        let reply: Value = serde_json::from_slice(&bytes).context("IDL service: not JSON")?;
        let read = |source: &str| -> Result<Option<Published>> {
            let entries = reply
                .get(source)
                .and_then(Value::as_array)
                .map(Vec::as_slice)
                .unwrap_or_default();
            let Some(entry) = entries
                .iter()
                .find(|entry| entry.get("content").is_some_and(|c| !c.is_null()))
            else {
                return Ok(None);
            };
            if entry.get("valid").and_then(Value::as_bool) == Some(false) {
                bail!("IDL service: the {source} IDL does not decode");
            }
            let raw = entry["content"]
                .as_str()
                .context("IDL service: content is not a string")?;
            let document = serde_json::from_str(raw)
                .with_context(|| format!("IDL service: the {source} IDL is not JSON"))?;
            Ok(Some(Published {
                raw: raw.to_owned(),
                document,
            }))
        };
        Ok(PublishedIdls {
            anchor: read("anchor")?,
            pmp: read("pmp")?,
        })
    }
}

/// Sends with retries on rate limiting, server errors and transport failures. A 4xx other than
/// 429 is returned as is: it will not change on retry.
fn send(request: RequestBuilder, attempts: u32) -> Result<(u16, Vec<u8>)> {
    let mut last = String::from("request failed");
    for attempt in 0..attempts {
        let request = request
            .try_clone()
            .expect("requests without a streaming body clone");
        match request.send() {
            Ok(response) => {
                let status = response.status().as_u16();
                if status == 429 || status >= 500 {
                    last = format!("HTTP {status}");
                } else {
                    let mut bytes = Vec::new();
                    response
                        .take(MAX_RESPONSE_BYTES + 1)
                        .read_to_end(&mut bytes)
                        .context("reading the response")?;
                    if bytes.len() as u64 > MAX_RESPONSE_BYTES {
                        bail!("the response exceeds {MAX_RESPONSE_BYTES} bytes");
                    }
                    return Ok((status, bytes));
                }
            }
            Err(error) => last = error.without_url().to_string(),
        }
        thread::sleep(Duration::from_secs(1 << attempt));
    }
    bail!(last)
}
