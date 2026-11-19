//! Reads the IDL of a program that publishes it on GitHub.
//!
//! Some programs are upgraded without their on-chain IDL being written again, and publish the IDL
//! of each release in a repository instead. For those the file there is the one to compare with,
//! and the on-chain copy is not read: it describes a program that has since moved on.
//!
//! Such a file can describe a release before it is deployed. An IDL ahead of the deployed program
//! does not decode the live accounts, so a committed IDL that differs from it is to be updated
//! once the protocol's integration tests pass with the new one.

use anyhow::{Context, Result, bail};
use reqwest::blocking::Client;

use crate::{
    http,
    idl_service::{Published, PublishedIdls},
};

pub const GITHUB: &str = "github";

/// The programs compared with a file on GitHub, and that file.
const IDLS: &[(&str, &str)] = &[
    (
        "6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P",
        "https://raw.githubusercontent.com/pump-fun/pump-public-docs/main/idl/pump.json",
    ),
    (
        "pAMMBay6oceH9fJKBRHGP5D4bD4sWpmSwMn52FMfXEA",
        "https://raw.githubusercontent.com/pump-fun/pump-public-docs/main/idl/pump_amm.json",
    ),
];

/// The file `program_id` publishes its IDL in, for a program that publishes it on GitHub.
pub fn idl_url(program_id: &str) -> Option<&'static str> {
    IDLS.iter()
        .find(|(listed, _)| *listed == program_id)
        .map(|(_, url)| *url)
}

pub struct GitHub {
    client: Client,
}

impl GitHub {
    pub fn new() -> Result<Self> {
        Ok(Self {
            client: http::client()?,
        })
    }

    /// The IDL the file at `url` holds right now.
    pub fn latest(&self, url: &str) -> Result<PublishedIdls> {
        let (status, bytes) = http::send(self.client.get(url)).context("GitHub")?;
        if status != 200 {
            bail!("GitHub: HTTP {status}");
        }
        let published = String::from_utf8(bytes)
            .context("the github IDL is not text")
            .and_then(|raw| Published::read(raw, GITHUB));
        Ok(PublishedIdls::from([(
            GITHUB,
            published.map_err(|error| format!("{error:#}")),
        )]))
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;
    use crate::check;

    #[test]
    fn only_the_listed_programs_are_read_from_github() {
        let url = idl_url("pAMMBay6oceH9fJKBRHGP5D4bD4sWpmSwMn52FMfXEA").unwrap();
        assert!(url.ends_with("/idl/pump_amm.json"), "{url}");
        assert_eq!(idl_url("KLend2g3cP87fffoy8q1mQqGKjrxjC8boSyAYavgmjD"), None);
    }

    /// A program listed under an address no committed IDL names would be compared with its
    /// on-chain IDL after all, without a word.
    #[test]
    fn every_listed_program_is_one_a_committed_idl_names() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let protocols = check::discover(&root).unwrap();
        for (program_id, _) in IDLS {
            let committed = protocols
                .iter()
                .any(|protocol| protocol.address.as_deref() == Some(program_id));
            assert!(committed, "no committed idl.json names {program_id}");
        }
    }
}
