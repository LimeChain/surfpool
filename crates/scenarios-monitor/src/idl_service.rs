//! Reads the IDLs a program publishes from the Solana IDL service (https://idl.solana.com/docs).

use std::{io::Read, thread, time::Duration};

use anyhow::{Context, Result, bail};
use reqwest::blocking::{Client, RequestBuilder};
use serde_json::Value;

const USER_AGENT: &str = "surfpool-scenarios-monitoring";
const BASE_URL: &str = "https://idl.solana.com";
/// The service answers in a second or two; a request that takes this long is not going to.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const ATTEMPTS: u32 = 3;
/// Upstream IDLs are a few MB at most; anything far larger is not an IDL.
const MAX_RESPONSE_BYTES: u64 = 32 * 1024 * 1024;

/// The two places a program publishes an IDL: its Anchor IDL account, and Program Metadata.
pub const ANCHOR: &str = "anchor";
pub const PROGRAM_METADATA: &str = "pmp";

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
    /// Why a source that publishes something could not be read, one line per such source.
    pub unreadable: Vec<String>,
}

impl PublishedIdls {
    pub fn iter(&self) -> impl Iterator<Item = (&'static str, Option<&Published>)> {
        [
            (ANCHOR, self.anchor.as_ref()),
            (PROGRAM_METADATA, self.pmp.as_ref()),
        ]
        .into_iter()
    }
}

pub struct IdlService {
    client: Client,
}

impl IdlService {
    pub fn new() -> Result<Self> {
        let client = Client::builder()
            .user_agent(USER_AGENT)
            .timeout(REQUEST_TIMEOUT)
            .build()?;
        Ok(Self { client })
    }

    /// The IDLs `program_id` publishes on mainnet right now.
    pub fn latest(&self, program_id: &str) -> Result<PublishedIdls> {
        let request = self
            .client
            .get(format!("{BASE_URL}/api/latest"))
            .query(&[("programId", program_id), ("cluster", "mainnet-beta")]);
        let (status, bytes) = send(request)?;
        if status != 200 {
            bail!("IDL service: HTTP {status}");
        }
        let reply: Value = serde_json::from_slice(&bytes).context("IDL service: not JSON")?;
        published_idls(&reply)
    }
}

/// Reads each source of a reply on its own, so that one source that cannot be read does not
/// cost the other. Fails only when nothing that is published can be read.
fn published_idls(reply: &Value) -> Result<PublishedIdls> {
    let mut published = PublishedIdls::default();
    for source in [ANCHOR, PROGRAM_METADATA] {
        let read = match read_source(reply, source) {
            Ok(read) => read,
            Err(error) => {
                published.unreadable.push(format!("{error:#}"));
                None
            }
        };
        match source {
            ANCHOR => published.anchor = read,
            _ => published.pmp = read,
        }
    }
    if published.anchor.is_none() && published.pmp.is_none() && !published.unreadable.is_empty() {
        bail!("IDL service: {}", published.unreadable.join("; "));
    }
    Ok(published)
}

fn read_source(reply: &Value, source: &str) -> Result<Option<Published>> {
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
        bail!("the {source} IDL does not decode");
    }
    let raw = entry["content"]
        .as_str()
        .with_context(|| format!("the {source} IDL is not text"))?;
    let document =
        serde_json::from_str(raw).with_context(|| format!("the {source} IDL is not JSON"))?;
    Ok(Some(Published {
        raw: raw.to_owned(),
        document,
    }))
}

/// Sends with retries on rate limiting, server errors and transport failures, including one
/// while reading the body. A 4xx other than 429 is returned as is: it will not change on retry.
fn send(request: RequestBuilder) -> Result<(u16, Vec<u8>)> {
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
    bail!("IDL service: {last}")
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn entry(content: &str, valid: bool) -> Value {
        json!([{"type": "x", "version": "1.0.0", "valid": valid, "content": content}])
    }

    #[test]
    fn each_source_is_read_on_its_own() {
        let reply = json!({"anchor": entry("{\"version\":\"1.0.0\"}", true), "pmp": []});
        let published = published_idls(&reply).unwrap();
        assert_eq!(published.anchor.unwrap().document["version"], "1.0.0");
        assert!(published.pmp.is_none());
        assert!(published.unreadable.is_empty());
    }

    #[test]
    fn a_source_that_cannot_be_read_does_not_cost_the_other() {
        let reply = json!({"anchor": entry("{}", true), "pmp": entry("{}", false)});
        let published = published_idls(&reply).unwrap();
        assert!(published.anchor.is_some());
        assert!(published.pmp.is_none());
        assert_eq!(published.unreadable, ["the pmp IDL does not decode"]);
    }

    #[test]
    fn nothing_readable_among_what_is_published_is_an_error() {
        let reply = json!({"anchor": entry("not json", true), "pmp": entry("{}", false)});
        let error = published_idls(&reply).unwrap_err().to_string();
        assert!(error.contains("the anchor IDL is not JSON"), "{error}");
        assert!(error.contains("the pmp IDL does not decode"), "{error}");
    }

    #[test]
    fn a_program_that_publishes_nothing_is_not_an_error() {
        let published = published_idls(&json!({"anchor": [], "pmp": []})).unwrap();
        assert!(published.anchor.is_none() && published.pmp.is_none());
        assert!(published.unreadable.is_empty());
    }
}
