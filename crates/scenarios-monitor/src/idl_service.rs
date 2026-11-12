//! Reads the IDLs a program publishes from the Solana IDL service (https://idl.solana.com/docs).

use std::{collections::BTreeMap, io::Read, thread, time::Duration};

use anyhow::{Context, Result, bail};
use reqwest::blocking::{Client, RequestBuilder};
use serde_json::Value;

const USER_AGENT: &str = "surfpool-scenarios-monitoring";
const BASE_URL: &str = "https://idl.solana.com";
const CLUSTER: &str = "mainnet-beta";
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

/// What one program publishes right now, by source (`ANCHOR` or `PROGRAM_METADATA`): the IDL,
/// or why it could not be read. A source that publishes nothing is left out.
pub type PublishedIdls = BTreeMap<&'static str, Result<Published, String>>;

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
            .query(&[("programId", program_id), ("cluster", CLUSTER)]);
        let (status, bytes) = send(request)?;
        if status != 200 {
            bail!("IDL service: HTTP {status}");
        }
        let reply: Value = serde_json::from_slice(&bytes).context("IDL service: not JSON")?;
        published_idls(&reply)
    }
}

/// The service's page for `program_id`, which shows what each source publishes side by side.
pub fn page_url(program_id: &str) -> String {
    format!("{BASE_URL}/?programId={program_id}&mode=latest&cluster={CLUSTER}")
}

/// Reads each source of a reply on its own, so that one source that cannot be read does not
/// cost the other. Fails when the reply does not list both sources.
fn published_idls(reply: &Value) -> Result<PublishedIdls> {
    let mut published = PublishedIdls::new();
    for source in [ANCHOR, PROGRAM_METADATA] {
        // The service lists both sources for every program, empty where it publishes none.
        // Taking a reply without one for "publishes nothing" would pass every protocol.
        let entries = reply
            .get(source)
            .and_then(Value::as_array)
            .with_context(|| format!("IDL service: the reply has no `{source}` list"))?;
        if let Some(read) = read_source(entries, source).transpose() {
            published.insert(source, read.map_err(|error| format!("{error:#}")));
        }
    }
    Ok(published)
}

fn read_source(entries: &[Value], source: &str) -> Result<Option<Published>> {
    // The service lists a source's live IDL alone, or nothing where it publishes none.
    let Some(entry) = entries.first() else {
        return Ok(None);
    };
    if entry.get("valid").and_then(Value::as_bool) == Some(false) {
        bail!("the {source} IDL does not decode");
    }
    let raw = entry
        .get("content")
        .and_then(Value::as_str)
        .with_context(|| format!("the {source} IDL has no content"))?;
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
        let anchor = published[ANCHOR].as_ref().unwrap();
        assert_eq!(anchor.document["version"], "1.0.0");
        assert!(!published.contains_key(PROGRAM_METADATA));
    }

    #[test]
    fn a_source_that_cannot_be_read_keeps_its_reason_and_does_not_cost_the_other() {
        let reply = json!({"anchor": entry("{}", true), "pmp": entry("{}", false)});
        let published = published_idls(&reply).unwrap();
        assert!(published[ANCHOR].is_ok());
        let reason = published[PROGRAM_METADATA].as_ref().unwrap_err();
        assert_eq!(reason, "the pmp IDL does not decode");

        let reply = json!({"anchor": entry("not json", true), "pmp": []});
        let published = published_idls(&reply).unwrap();
        let reason = published[ANCHOR].as_ref().unwrap_err();
        assert!(reason.starts_with("the anchor IDL is not JSON"), "{reason}");
    }

    #[test]
    fn an_entry_without_content_is_not_taken_for_publishing_nothing() {
        for entry in [
            json!({"type": "pmp", "valid": true}),
            json!({"type": "pmp", "valid": true, "content": null}),
            json!({"type": "pmp", "valid": true, "idl": "{}"}),
        ] {
            let published = published_idls(&json!({"anchor": [], "pmp": [entry]})).unwrap();
            let reason = published[PROGRAM_METADATA].as_ref().unwrap_err();
            assert_eq!(reason, "the pmp IDL has no content");
        }
    }

    #[test]
    fn a_reply_that_does_not_list_both_sources_is_an_error() {
        for reply in [
            json!({"anchor": entry("{}", true)}),
            json!({"anchor": [], "pmp": null}),
            json!({"error": "not found"}),
        ] {
            let error = published_idls(&reply).unwrap_err().to_string();
            assert!(error.contains("the reply has no `"), "{error}");
        }
    }

    #[test]
    fn a_program_that_publishes_nothing_is_not_an_error() {
        let published = published_idls(&json!({"anchor": [], "pmp": []})).unwrap();
        assert!(published.is_empty());
    }
}
