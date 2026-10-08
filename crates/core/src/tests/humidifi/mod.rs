//! HumidiFi v8 raw-layout and deployed-program tests.
//!
//! HumidiFi publishes no IDL and checks its caller through the Instructions sysvar, so the swap
//! proofs replay a captured DFlow route against the deployed ELF in LiteSVM after driving the shipped
//! templates through `materialize_raw_layout` or the Surfnet materializer.

use std::{collections::HashMap, sync::Arc};

use litesvm::LiteSVM;
use solana_account::Account;
use solana_commitment_config::CommitmentConfig;
use solana_instruction::{AccountMeta, Instruction, error::InstructionError};
use solana_message::Message;
use solana_program_pack::Pack;
use solana_program_runtime::{
    declare_process_instruction, solana_sbpf::program::BuiltinFunctionDefinition,
};
use solana_pubkey::Pubkey;
use solana_signature::Signature;
use solana_transaction::Transaction;
use solana_transaction_error::TransactionError;
use surfpool_types::{AccountAddress, OverrideInstance, Scenario};

use crate::{
    scenarios::TemplateRegistry,
    surfnet::{GetAccountResult, remote::SurfnetRemoteClient, svm::SurfnetSvm},
    tests::helpers::diff_indices,
};

const RPC_URL_ENV: &str = "SURFPOOL_TEST_RPC_URL";
const DEFAULT_RPC_URL: &str = "https://api.mainnet-beta.solana.com";
const PROGRAM: Pubkey = Pubkey::from_str_const("9H6tua7jkLhdm3w8BvgpTn5LZNU7g4ZynDmCiNN3q6Rp");
const PROGRAMDATA: Pubkey = Pubkey::from_str_const("G9S64i58RRWJA28vZiNhnP56Ux4Ef7hfMgHNREnZZSom");
const DEPLOYED_SLOT: u64 = 449592669;

const MARKET_SIZE: usize = 1728;
const LAYOUT_TAG: [u8; 8] = [44, 90, 19, 124, 56, 111, 47, 150];
const FAIR_VALUE_MASK: u64 = 0xb957_ed15_dc87_7426;
const SLOT_MASK: u64 = 0x6e9d_e2b3_0b19_f1ea;
const PUBKEY_MASKS: [u64; 4] = [
    0xfb5c_e87a_ae44_3c38,
    0x04a2_1784_51ba_c3c7,
    0x04a1_1787_51b9_c3c6,
    0x04a0_1786_51b8_c3c5,
];
const STALE_QUOTE_ERROR: u32 = 0xfaded;
// The fixtures refuse an empty payout vault themselves; some stopped markets leave it to the token
// transfer, which fails with InsufficientFunds instead.
const EMPTY_PAYOUT_ERROR: u32 = 49;
const FAIR_VALUE: std::ops::Range<usize> = 576..584;
const MAX_STALENESS: std::ops::Range<usize> = 608..616;
const LAST_UPDATE: std::ops::Range<usize> = 616..624;
const AMOUNT: std::ops::Range<usize> = 64..72;
const MAKER_SPREAD_MASK: u64 = 0x5041_56a2_2548_f8dc;
const TIER_SPREADS: [(usize, u64); 3] = [
    (176, 0x40f8_49d0_0057_07ba),
    (256, 0x40f2_49da_005d_07b4),
    (336, 0x40e4_49cc_004b_07ae),
];
const SPREAD_WORDS: [std::ops::Range<usize>; 4] = [800..808, 176..184, 256..264, 336..344];

async fn fetch(addresses: &[Pubkey]) -> Vec<Account> {
    let client = SurfnetRemoteClient::new(
        std::env::var(RPC_URL_ENV).unwrap_or_else(|_| DEFAULT_RPC_URL.to_string()),
    );
    let mut out = Vec::new();
    for batch in addresses.chunks(100) {
        let mut attempt = 0;
        let results = loop {
            match client
                .get_multiple_accounts(batch, CommitmentConfig::confirmed())
                .await
            {
                Ok(v) => break v,
                Err(e) => {
                    attempt += 1;
                    assert!(attempt < 5, "fetch {batch:?}: {e}");
                    tokio::time::sleep(std::time::Duration::from_millis(750 * attempt)).await;
                }
            }
        };
        out.extend(
            results
                .into_iter()
                .zip(batch)
                .map(|(result, address)| match result {
                    GetAccountResult::FoundAccount(_, account, _)
                    | GetAccountResult::FoundCoupledAccount((_, account), _, _) => account,
                    GetAccountResult::None(_) => panic!("{address} no longer exists"),
                }),
        );
    }
    out
}

fn word(data: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes(data[offset..offset + 8].try_into().unwrap())
}

fn masked_pubkey(data: &[u8], offset: usize) -> Pubkey {
    let mut bytes = [0u8; 32];
    for (i, mask) in PUBKEY_MASKS.iter().enumerate() {
        bytes[i * 8..(i + 1) * 8]
            .copy_from_slice(&(word(data, offset + i * 8) ^ mask).to_le_bytes());
    }
    Pubkey::new_from_array(bytes)
}

fn pubkey_at(data: &[u8], offset: usize) -> Pubkey {
    Pubkey::new_from_array(data[offset..offset + 32].try_into().unwrap())
}

fn amount(data: &[u8]) -> u64 {
    word(data, 64)
}

fn apply_raw(id: &str, data: &[u8], values: &[(&str, serde_json::Value)], slot: u64) -> Vec<u8> {
    let registry = TemplateRegistry::new();
    let t = registry.get(id).unwrap_or_else(|| panic!("missing {id}"));
    assert!(t.raw_layout, "{id} must use raw-layout writes");
    let map = values
        .iter()
        .map(|(k, v)| (k.to_string(), v.clone()))
        .collect::<HashMap<_, _>>();
    t.materialize_raw_layout(data, &map, slot)
        .unwrap_or_else(|e| panic!("{id}: {e}"))
}

// A masked write can leave a byte equal to the original, so only "nothing outside moved" holds.
fn assert_only_within(
    before: &[u8],
    after: &[u8],
    ranges: &[std::ops::Range<usize>],
    context: &str,
) {
    assert_eq!(before.len(), after.len(), "{context}: length changed");
    for i in diff_indices(before, after) {
        assert!(
            ranges.iter().any(|r| r.contains(&i)),
            "{context}: byte {i} changed outside {ranges:?}"
        );
    }
}

fn values(entries: &[(&str, serde_json::Value)]) -> HashMap<String, serde_json::Value> {
    entries
        .iter()
        .map(|(key, value)| ((*key).to_string(), value.clone()))
        .collect()
}

#[derive(Clone, Copy)]
struct MarketDef {
    pair: &'static str,
    market: Pubkey,
    base_mint: Pubkey,
    quote_mint: Pubkey,
    base_vault: Pubkey,
    quote_vault: Pubkey,
}

/// The markets Studio offers as shortcuts, in its order. The behavioural tests replay the ones
/// whose vaults are classic SPL-token accounts.
const FEATURED_MARKETS: [MarketDef; 4] = [
    MarketDef {
        pair: "SOL / USDC",
        market: Pubkey::from_str_const("8sKQHfjNhvmAw94PhfvfMcytmqW6jmxvwieYyzXCCPu"),
        base_mint: Pubkey::from_str_const("So11111111111111111111111111111111111111112"),
        quote_mint: Pubkey::from_str_const("EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v"),
        base_vault: Pubkey::from_str_const("H292B1VbSvD6GuUmSvUvfQstg1Acfzog796uQ7d1ccCw"),
        quote_vault: Pubkey::from_str_const("A3C9xwv4Hfx92M5HQpxUiibSqCa5pYhD2kTwnU5fEPq"),
    },
    MarketDef {
        pair: "HYPE / USDC",
        market: Pubkey::from_str_const("H3TyE2Q3rDrvRXD8PzHYE7BS2hafGuybje4qXCtyWqMH"),
        base_mint: Pubkey::from_str_const("98sMhvDwXj1RQi5c5Mndm3vPe9cBqPrbLaufMXFNMh5g"),
        quote_mint: Pubkey::from_str_const("EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v"),
        base_vault: Pubkey::from_str_const("49pft8q7cyugVDWnJCXjMHJdb5Uji198k5MognVvA48n"),
        quote_vault: Pubkey::from_str_const("EKFwmKoPA9o3HpoL8AnQd8uNBxtJbaPv4w4tbxNn6iei"),
    },
    MarketDef {
        pair: "JUP / USDC",
        market: Pubkey::from_str_const("hKgG7iEDRFNsJSwLYqz8ETHuZwzh6qMMLow8VXa8pLm"),
        base_mint: Pubkey::from_str_const("JUPyiwrYJFskUPiHa7hkeR8VUtAeFoSYbKedZNsDvCN"),
        quote_mint: Pubkey::from_str_const("EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v"),
        base_vault: Pubkey::from_str_const("GRLhXC9eKoZMLBNQqRnNHg62hc4NM2Wm4amm7Aiz4ts"),
        quote_vault: Pubkey::from_str_const("AKaBxaHDdaJEJyQSHNRdLu7yN3JbDDkSG189ssTjSGW8"),
    },
    MarketDef {
        pair: "PUMP / USDC",
        market: Pubkey::from_str_const("HyKbc1vUxNL9xJC2Q44qdGiuKC1Ey95KNdokfHivrFnZ"),
        base_mint: Pubkey::from_str_const("pumpCmXqMfrsAkQ5r49WcJnRayYRqmXz6ae8H7H9Dfn"),
        quote_mint: Pubkey::from_str_const("EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v"),
        base_vault: Pubkey::from_str_const("7CdAWd32k1c9ywpyErij2xRbBDRCLFqrZAXTUjD3ziek"),
        quote_vault: Pubkey::from_str_const("Cad4XrQrtXdKEhZKtWX5YAkwumUvphZKw1cqpYXYnZDr"),
    },
];

/// Every schema-8 market the program owns, active and stopped, as (market, base vault, quote vault)
/// unmasked from the market. Each template must write only its own bytes on every one of them.
const ALL_MARKETS: [(&str, &str, &str); 37] = [
    (
        "2866MvCKPGz9LdnPcmPueoV3mA2Ac1ceEQ8Xqb9VNefu",
        "8UwYWkJ78uG7Cy8bZJURzwFfDUTX4oeiAriBQVWwmY8",
        "6iNgLk2CUJYA6ZVTcaTeSQiaaUfUbYgZZShWCSsTTkPn",
    ),
    (
        "3QYYvFWgSuGK8bbxMSAYkCqE8QfSuFtByagnZAuekia2",
        "AzzKL5BpnX9UXfJRyiGbAwyFvw93vwCTBvW56tJebsqd",
        "EmyNiSYtMYZsBCVYpzLwNduQFqCpDGR8qM1h2CoLpWnM",
    ),
    (
        "3xgq5NN4yCmxJwZh4C4574a45YdAwbQ2N91Wk6TdUK7e",
        "mX93mQi84N8agH8Yw7cnLkZKYixTM7HeLuaH9FzL7Cn",
        "6iH2wzUwjiKAJn91RTN8npneZ7GX2N1tpWw14RDuFVX",
    ),
    (
        "4meJdPuPQqKS8XiP3sNzJvkneDKgHf86yYSUtALhSVke",
        "7sKvQwkPguDhPEwBaeKY7YacAMpcmXxHu7CNkr3giw76",
        "Av7qM1Pfweoy4mvw6eoGdCzvjR7EJHLiF8zwf2VNJX64",
    ),
    (
        "5dhYayH9qvzNyCPoh2hKN8TJqumoGsWdyZ9UfPLXBfD9",
        "86DHdQfRpghMCmXYKDg93bXtmi3doBL5NeY9DYwAj6rz",
        "FqEVwrQJsJ4AciwJRYLPQEdstDfUYJp6z3f6XYb6MnSb",
    ),
    (
        "6NRaKmLM2tg6gdoN8eup1RmzDR9qXpvqsACjkH5dgtWP",
        "82ctDdjbkGTW2HK5LNED1WyjJAdWQ3VRgxAj1SQFQ6z6",
        "6LrDwGubqfV68iPFf2i94k7aKhYWT6vwsSzECHuCVs8",
    ),
    (
        "6kWQ3q8akoeMmrfJX4s76jfHuf4DAMGNYoHwDUR4kJwN",
        "iXKEpsNhJc81MkMUZXe5LRnyC7Z9nr8rt6Wymt5Ktpq",
        "HXY8cZjnfTMpGfq7Lotor7fdwqf8bq6yToexKcRY6g1a",
    ),
    (
        "6n9VhCwQ7EwK6NqFDjnHPzEk6wZdRBTfh43RFgHQWHuQ",
        "Cv9St5tDTGwpbG5UVvM6QvFmf3FYSXc14W9BYvQN5wAZ",
        "7Rf8Gu8YemSoGjZT3z1cL5BT9HLbGywcyaz8Mrbhd1MH",
    ),
    (
        "755egiJPoXPPAnHoyanocrdfTJUirMi6Va64Gi6N1dvA",
        "D9UUwqmno3ZR4mu5Hiym44df8Hpk5MYiftcwwHYnD4bf",
        "Csmmz9Na9dHQzfdFqbWstcWprH7PoVpAMpMeFeCK1TPk",
    ),
    (
        "7KdoBm9vrujoH8nhLD3YfaJ7oqentYVNFe7K4KR9jcyb",
        "KndqB2YnFyE7pmMLwrjz7sUmu4ssKkNZYgZFWhz6xM2",
        "GLzuwfLeyLFMgerqAqh5b6bNPPtDViUGGhvz8qxRNvr1",
    ),
    (
        "7XoMov9LfxHDrqRw1vSzXszb8k35K85WC9tQKSqpyv1M",
        "FzzpuhKbY2LekZuiuQxAmMAjUT6fWLrjQB85b9KhbDx7",
        "7qgwzoj4z8zkqQMLSmojCqQU9Hr63xQdmHLZgX58WHKS",
    ),
    (
        "8Re9ESfUWCTKKZ5iKQW4WB2DLsDjPyT2ifzQUopQ5BAs",
        "G16H4Pizaxaczpd2W5hA7kdoW2FfyYmWDLpsm1hsaPM2",
        "HV1YwHhkTu1RgBN9CzVVbCYniATTdzp1jk3gcfrZrtLT",
    ),
    (
        "8WFduUYU7iX94E3ZMejpTXi5TadKh9j5qp5ez5uSBJwa",
        "BShGd3SBaP349y4Krp6EEYhtRzSGYBBe8GU8LFUXMU5C",
        "HR8LDFWztXB6YA9Dvg27LHNSZAhwaGFihRD3GWKy7AUb",
    ),
    (
        "8sKQHfjNhvmAw94PhfvfMcytmqW6jmxvwieYyzXCCPu",
        "H292B1VbSvD6GuUmSvUvfQstg1Acfzog796uQ7d1ccCw",
        "A3C9xwv4Hfx92M5HQpxUiibSqCa5pYhD2kTwnU5fEPq",
    ),
    (
        "9PJ5x3gCsWS84YfKwfkt41SHndeCMUSshzHDkZpMXyc7",
        "H3aArwg5Fz126mtJLRumnGBpvoZqaH6uqJPYG2ZsTu2V",
        "wTSyYgXTFpTBYxgk5DRiFsyoWzM5NXXwV8gUMY4nUq6",
    ),
    (
        "9c5xYTnURgpQLDk4XqkJdaUab6p8EMBgE5n7n29pQzCy",
        "463wgaEw5WCjZ8R99j3Bw3EiY6GvtdTVrNz6ztM2ny1Z",
        "DtSsQf6JjRFVNfko8h3hotNaGsJLH9yWVNpaxbE5DuyY",
    ),
    (
        "9iVWeHzmfQMa45jDVEDAHBjS5nJrWQEdQidjD1SVjEhy",
        "2wAmF3B7ukhDYUrRY4Us2bX7oZxbMDxhyv3baquhGTX4",
        "8WBgsDcNt9ecxhj7dZrW8k1kgJngTU2Z15Y1xRJfBZbe",
    ),
    (
        "9xhymMTGDG9U79UPTvSAMz4QfdwCid6cdp7VoNv4wCEk",
        "13EU2j9gAWQWtFf55HHGS7CcRW64qE184ndHjhLxyuFJ",
        "4E1JKb28xKH4ff7FUXSRB4VCK8rVucvYD6CagDbuViBs",
    ),
    (
        "AjotvEmqbVWu9UPt3DKCiTgVxRxZYcVu99XshPdws9go",
        "E1QG5wNp8nsS1JwVbsJJDCTiYZkMiVzgALniEHeWcRAN",
        "HMrT9PYDgxVHTAVxQYV1uC7gcfwdshKDZDAiFQHkQkYe",
    ),
    (
        "AvGeFw71N5sNfV97mZ1uNrHg4yfufRicCJUrS9j2ehTX",
        "ECEPWwZJ1U1Vjsj1X5sUbZYETKMSCjYHuoTMVitCn64t",
        "FBWtVVvzsRuAAzVX8ua1hden9KmgPrC2rFijuwEn1ngJ",
    ),
    (
        "BtDQz6LSEj24VRNU4qCnxngdPZVNAWHdERsMb1tRYfv5",
        "BKyZQYSPkzAt6C4uYLq36wYD2pqpjxHtov7okc6jFHfv",
        "F2abAUrD5vHDFRnKZqjZH1udbGWmgsSpsSC9Eg7UrRi9",
    ),
    (
        "Cc2oMdeNDmZ7ZD8b7igQg1EHjBEinR1Vi6nx2VHpVm6H",
        "7gxM8fzfJ331g1yddEDUDrz4BLwEEPPPcCDV57pDQWyT",
        "F9JdjxCTiyy9xB4BjvGQAzzxetVJQWAjWzAMxzMMD23D",
    ),
    (
        "DB3sUCP2H4icbeKmK6yb6nUxU5ogbcRHtGuq7W2RoRwW",
        "8BrVfsvzb1DZqCactbYWoKSv24AfsLBuXJqzpzYCwznF",
        "HsQcHFFNUVTp3MWrXYbuZchBNd4Pwk8636bKzLvpfYNR",
    ),
    (
        "DvUdNir9myWcXc4aqD2T2UTSL8xyAj8jTY5XnodcT33k",
        "7ZzeDr99ZrBmyAhS97BdUsSxuvQ4GfgPUzr6ghivaKtK",
        "7eetXLpfUT3qYqtXa568mQLDjZrvbz3pNe4izbTwNRzM",
    ),
    (
        "ELp3rjbAncW8FLAPvmWS6vjG4FXFLDo1HiKh4PhQBgvx",
        "4uGxv6uabyEmHzhi7LBWGzqSwGEE98VzYL2NahRqbfCP",
        "2XCEJJf3hnuMeot5dJST9yaj92i1wCLB2c67eH7WYzvR",
    ),
    (
        "FAYL7CoNENA6jGCWVJb5MZ7mjKuiiyYRkDTopdVticpp",
        "9dt86PLMhntCBm1jpQCi6zsFTtGBR7rvVEGhdk3pp1T5",
        "FpqzUjo7zDj7EdDn336qZaaisvxBpMGkd27mmRtVLgDt",
    ),
    (
        "Fhjzcf3JFH3zN6uKoNwwr9WhfdUnFoCY9KZKybx3CoR9",
        "DrmUUH2qoB4wpurb44i4NyZpSmfjVRdUswDakUt9JNpM",
        "DVUBW86zKoEgXm6qJDCwMFMnYiXG6qvz7G1Tdvci7xut",
    ),
    (
        "FksffEqnBRixYGR791Qw2MgdU7zNCpHVFYBL4Fa4qVuH",
        "C3FzbX9n1YD2dow2dCmEv5uNyyf22Gb3TLAEqGBhw5fY",
        "3RWFAQBRkNGq7CMGcTLK3kXDgFTe9jgMeFYqk8nHwcWh",
    ),
    (
        "GAcKqojqgkRVDBihVAGvbqnuZdCiN9UGNrPCRG5DMGGA",
        "5g9P9hb7HRnkp5LLoGEraxWV6DVRJ3tLxTpnbTTthaBK",
        "7ZyaWi7WQCCfEnAZ1z3PQuUq9zYkSyEuBYoYD8fzfkot",
    ),
    (
        "GD5eJhDmVcosRoU2H27LoFCuYERMTe4Mf1ooban6ba9T",
        "D5GQBX8uKAEY9rJAhYGwCviKDw7hWi2umxeGNHRt5oF8",
        "HXw3tHoQ5qNJbsVtwWHkvZb2daT9w2M3tpnqX8zLmEQ",
    ),
    (
        "H3TyE2Q3rDrvRXD8PzHYE7BS2hafGuybje4qXCtyWqMH",
        "49pft8q7cyugVDWnJCXjMHJdb5Uji198k5MognVvA48n",
        "EKFwmKoPA9o3HpoL8AnQd8uNBxtJbaPv4w4tbxNn6iei",
    ),
    (
        "HjKAdGgBZDW3kMyFLYzkykkirVtbJnMhi4n2oBVp8rLU",
        "FKe42A194oAWhqdSoZCrtTaaCnGusEr2pNwwj1dmHht7",
        "7Y2n2mDcAgmdzXNzubVLCKS8ofQ372gBT4npSth8KtJE",
    ),
    (
        "Htj37Rj9cqvcVgFgDU92GzTwgaeHF8QqwVe33uXKLJT8",
        "2Qu3LFqj9YGDi398rbRnx1f7RyZwAPYT2YPKhw7ZPqPr",
        "DXq59kJVGQqtyFKQURqVwf6MQsrhFLe7VMAV5TEfK5gY",
    ),
    (
        "HyKbc1vUxNL9xJC2Q44qdGiuKC1Ey95KNdokfHivrFnZ",
        "7CdAWd32k1c9ywpyErij2xRbBDRCLFqrZAXTUjD3ziek",
        "Cad4XrQrtXdKEhZKtWX5YAkwumUvphZKw1cqpYXYnZDr",
    ),
    (
        "PuxcQpbFrybkUAsqAKhJZ3NPgj5QG1i1FMhog6bDjx4",
        "7SqxZrrmcy7Nw9zRhv2haRWQWTkFevHoycHxXgYXfXN7",
        "DkDsnfd1KUYZJnsRHSYHE4z9pK9gdXjknTBnscNM75gr",
    ),
    (
        "hKgG7iEDRFNsJSwLYqz8ETHuZwzh6qMMLow8VXa8pLm",
        "GRLhXC9eKoZMLBNQqRnNHg62hc4NM2Wm4amm7Aiz4ts",
        "AKaBxaHDdaJEJyQSHNRdLu7yN3JbDDkSG189ssTjSGW8",
    ),
    (
        "iAMtZieUtpLwB3dzWw8Fo3H3FPkMFy3ej52URusseR1",
        "BaWYf3id5vL34Ag2Aa6n7eBXUdQ52cU2Yk5iXm7WyZBc",
        "HUg4WUuQoxoGvdDNvHpiy1jEC5VgzQdNzCNmestzV6mP",
    ),
];

// Instruction metadata captured from DFlow outer instruction 2 of tx 3zevqwAa8u136UGE1bdBzP1o2dpFuc7X333dC1ut3T1uCgY6iidihfNJNoJ7tuyj3tHNin1i7HTFCUSFWqK7g1Si (slot 444225745).
// It is a SOL/USDC buy; the replay swaps in each fixture's market, vaults and mints.
const DFLOW: Pubkey = Pubkey::from_str_const("DF1ow4tspfHX9JwWJsAb9epbkA8hmpSEAtxXy1V27QBH");
const SIGNER0: Pubkey = Pubkey::from_str_const("4HJaX8K9mH9fMLGn4Xc5DjGdjDWXNSnnX5kkXFuzk2ET");
const USER_BASE: Pubkey = Pubkey::from_str_const("CsqNfUnwbVbDQRQFUGCRCik8auK1ExfvwTYvM6Zc1uPe");
const USER_QUOTE: Pubkey = Pubkey::from_str_const("FMc3ZxJSYyT9JuJ21iDzQs5P2wRpS6JSwvT6ZRkYv6J9");
const CLOCK: Pubkey = Pubkey::from_str_const("SysvarC1ock11111111111111111111111111111111");
const TOKEN: Pubkey = Pubkey::from_str_const("TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA");
const SYS0: Pubkey = Pubkey::from_str_const("8xeaWCsJYxRoudEZGJWURdfrtFhLYZz9b4iHJnW5tb3d");
const AUX12: Pubkey = Pubkey::from_str_const("8vqruQc1wB3YQpaP4fr1woJGULBQG3c7uj8A8nnSWo9");
const VOTE: Pubkey = Pubkey::from_str_const("J1to1yufRnoWn81KYg1XkTWzmKjnYSnmE2VY8DGUJ9Qv");
const ROUTE_STATE: Pubkey = Pubkey::from_str_const("EXNBiVYTJErnLRaz9hae8P4nePswG21qZJZX9wJLUDnY");
const AUX15: Pubkey = Pubkey::from_str_const("7Qca6CS6sExGXKh3UmJ5fpapc3YFwScVpjfhmi4ScWUM");
const AUX16: Pubkey = Pubkey::from_str_const("6iL7bcqz6tmLo821xcvgDDSvrPh7knjoMQpyZEuSxML3");
const AUX17: Pubkey = Pubkey::from_str_const("1kUMdzAeH1uEdNch7ZJrtfvSo351p3b2DJ6qf5TUWhC");

const INPUT_QUOTE_AMOUNT: u64 = 2_427_890;
// Obfuscated by HumidiFi; it encodes the fixed quote input above.
const SWAP_DATA_HEX: &str = "6f86a350a21ac2b9c9f40bffe3baeac339ff2dffe0bae9c309";
// Kept verbatim so the Instructions sysvar shows a faithful router instruction.
const DFLOW_DATA_HEX: &str = "f8c69e91e17587c80600000025af1df3ba698aa144c79fb835e10649c7698eed21f706d1d24eb9a14f7dd0ce4c3fcd6752db35e4c5ec7f9c670b824cf28236bd4ab7af472aa63da2a26212570664597a1a000024bc9a000000323c31020000006a9991000000000006835faa1a00000000091f020000000381878ec3c80af75c0355798caf40a0297af20b25000000000001118e5964010000000000feb4a000000000003c020000";

/// How many featured markets the behavioural tests replay.
const FIXTURES: usize = 3;
/// A fixture's base vault holds at least this many times the replayed swap's output.
const VAULT_COVER: u64 = 10;

thread_local! {
    // The shim is a plain builtin with no arguments, so the replay tells it which market to route to.
    static ROUTE: std::cell::Cell<MarketDef> = const { std::cell::Cell::new(FEATURED_MARKETS[0]) };
}

// DFlow signs for SIGNER0 and SYS0 as PDAs; sigverify is off, so the replay marks them signers.
fn humidifi_metas(def: MarketDef) -> Vec<AccountMeta> {
    [
        (SIGNER0, true, true),
        (def.market, true, false),
        (def.base_vault, true, false),
        (def.quote_vault, true, false),
        (USER_BASE, true, false),
        (USER_QUOTE, true, false),
        (CLOCK, false, false),
        (TOKEN, false, false),
        (TOKEN, false, false),
        (SYS0, false, true),
        (def.base_mint, false, false),
        (def.quote_mint, false, false),
        (AUX12, false, false),
        (VOTE, false, false),
        (ROUTE_STATE, false, false),
        (AUX15, false, false),
        (AUX16, false, false),
        (AUX17, false, false),
    ]
    .into_iter()
    .map(|(key, writable, signer)| {
        if writable {
            AccountMeta::new(key, signer)
        } else {
            AccountMeta::new_readonly(key, signer)
        }
    })
    .collect()
}

// Registered under DFlow's id so HumidiFi's Instructions-sysvar caller check sees its router.
declare_process_instruction!(HumidiFiRouteShim, 1, |invoke_context| {
    invoke_context.native_invoke_signed(
        Instruction {
            program_id: PROGRAM,
            accounts: humidifi_metas(ROUTE.with(std::cell::Cell::get)),
            data: hex::decode(SWAP_DATA_HEX).unwrap(),
        },
        &[],
    )?;
    Ok(())
});

#[derive(Clone)]
struct HumidifiFixture {
    def: MarketDef,
    elf: Arc<Vec<u8>>,
    market: Account,
    base_vault: Account,
    quote_vault: Account,
    shared: Vec<(Pubkey, Account)>,
    /// A clock the upstream quote is fresh at, so price proofs are not staleness proofs.
    slot: u64,
    max_staleness: u64,
}

/// Base atoms the replayed quote input buys at `fair_value`, before spread.
fn expected_output(fair_value: u64) -> u64 {
    (u128::from(INPUT_QUOTE_AMOUNT) * (1u128 << 48) / u128::from(fair_value)) as u64
}

async fn fixtures() -> Arc<Vec<HumidifiFixture>> {
    static CACHE: tokio::sync::OnceCell<Arc<Vec<HumidifiFixture>>> =
        tokio::sync::OnceCell::const_new();
    CACHE
        .get_or_init(|| async {
            let programdata = fetch(&[PROGRAMDATA]).await.remove(0);
            assert!(
                programdata.data.len() > 300_000,
                "programdata is unexpectedly short"
            );
            let deployed_slot = u64::from_le_bytes(programdata.data[4..12].try_into().unwrap());
            assert_eq!(
                deployed_slot, DEPLOYED_SLOT,
                "program redeployed at slot {deployed_slot}; revalidate the raw offsets"
            );
            let elf = Arc::new(programdata.data[45..].to_vec());
            let mut out = Vec::new();
            for def in FEATURED_MARKETS {
                if out.len() == FIXTURES {
                    break;
                }
                let shared_keys = [def.base_mint, def.quote_mint, VOTE, ROUTE_STATE];
                let a = fetch(
                    &[
                        &[def.market, def.base_vault, def.quote_vault][..],
                        &shared_keys,
                    ]
                    .concat(),
                )
                .await;
                let classic_vaults = [&a[1], &a[2]]
                    .iter()
                    .all(|vault| vault.owner == TOKEN && vault.data.len() == 165);
                let upstream_fair_value = word(&a[0].data, 576) ^ FAIR_VALUE_MASK;
                if !classic_vaults
                    || upstream_fair_value == 0
                    || amount(&a[1].data)
                        < expected_output(upstream_fair_value).saturating_mul(VAULT_COVER)
                {
                    continue;
                }
                assert_eq!(
                    masked_pubkey(&a[0].data, 480),
                    def.base_vault,
                    "{}",
                    def.pair
                );
                assert_eq!(
                    masked_pubkey(&a[0].data, 448),
                    def.quote_vault,
                    "{}",
                    def.pair
                );
                out.push(HumidifiFixture {
                    def,
                    elf: elf.clone(),
                    slot: word(&a[0].data, 616) ^ SLOT_MASK,
                    max_staleness: word(&a[0].data, 608) ^ SLOT_MASK,
                    market: a[0].clone(),
                    base_vault: a[1].clone(),
                    quote_vault: a[2].clone(),
                    shared: shared_keys
                        .into_iter()
                        .zip(a[3..].iter().cloned())
                        .collect(),
                });
            }
            let first = out
                .first()
                .expect("no featured HumidiFi market has funded classic vaults");
            assert_eq!(
                first.def.pair, "SOL / USDC",
                "SOL / USDC must be the first fixture"
            );
            Arc::new(out)
        })
        .await
        .clone()
}

fn token_account(mint: Pubkey, owner: Pubkey, amount: u64) -> Account {
    let mut data = vec![0u8; spl_token_interface::state::Account::LEN];
    spl_token_interface::state::Account {
        mint,
        owner,
        amount,
        state: spl_token_interface::state::AccountState::Initialized,
        ..Default::default()
    }
    .pack_into_slice(&mut data);
    Account {
        lamports: 2_039_280,
        data,
        owner: TOKEN,
        ..Account::default()
    }
}

fn system_account(lamports: u64) -> Account {
    Account {
        lamports,
        owner: Pubkey::default(),
        ..Account::default()
    }
}

/// Swaps the captured quote input for base against the supplied market and vault bytes, returning
/// the base atoms the user received.
fn replay(
    fixture: &HumidifiFixture,
    clock_slot: u64,
    market: Vec<u8>,
    base_vault: Vec<u8>,
    quote_vault: Vec<u8>,
) -> Result<u64, TransactionError> {
    let mut svm = LiteSVM::new()
        .with_sigverify(false)
        .with_blockhash_check(false);
    svm.add_program(PROGRAM, &fixture.elf)
        .expect("load HumidiFi ELF");
    svm.add_builtin(DFLOW, HumidiFiRouteShim::register);
    let mut clock: solana_clock::Clock = svm.get_sysvar();
    clock.slot = clock_slot;
    clock.unix_timestamp = 1_788_000_000;
    svm.set_sysvar(&clock);

    for (key, template, data) in [
        (fixture.def.market, &fixture.market, market),
        (fixture.def.base_vault, &fixture.base_vault, base_vault),
        (fixture.def.quote_vault, &fixture.quote_vault, quote_vault),
    ] {
        svm.set_account(
            key,
            Account {
                data,
                ..template.clone()
            },
        )
        .unwrap();
    }
    for (key, account) in &fixture.shared {
        svm.set_account(*key, account.clone()).unwrap();
    }
    svm.set_account(SIGNER0, system_account(1_000_000_000))
        .unwrap();
    svm.set_account(SYS0, system_account(1_244_010)).unwrap();
    for aux in [AUX12, AUX15, AUX16, AUX17] {
        svm.set_account(aux, system_account(1_000_000)).unwrap();
    }
    svm.set_account(
        USER_QUOTE,
        token_account(fixture.def.quote_mint, SIGNER0, 1_000_000_000),
    )
    .unwrap();
    svm.set_account(USER_BASE, token_account(fixture.def.base_mint, SIGNER0, 0))
        .unwrap();

    let mut accounts = vec![AccountMeta::new_readonly(PROGRAM, false)];
    accounts.extend(humidifi_metas(fixture.def));
    let route = Instruction {
        program_id: DFLOW,
        accounts,
        data: hex::decode(DFLOW_DATA_HEX).unwrap(),
    };
    let compute_limit = Instruction {
        program_id: Pubkey::from_str_const("ComputeBudget111111111111111111111111111111"),
        accounts: vec![],
        data: [vec![2], 1_400_000u32.to_le_bytes().to_vec()].concat(),
    };
    let message = Message::new_with_blockhash(
        &[compute_limit, route],
        Some(&SIGNER0),
        &svm.latest_blockhash(),
    );
    let tx = Transaction {
        signatures: vec![Signature::default(); message.header.num_required_signatures as usize],
        message,
    };
    ROUTE.with(|route| route.set(fixture.def));
    svm.send_transaction(tx).map_err(|failed| failed.err)?;
    assert_eq!(
        1_000_000_000 - amount(&svm.get_account(&USER_QUOTE).unwrap().data),
        INPUT_QUOTE_AMOUNT,
        "the captured instruction's quote input changed"
    );
    Ok(amount(&svm.get_account(&USER_BASE).unwrap().data))
}

fn run(
    fixture: &HumidifiFixture,
    clock_slot: u64,
    market: Vec<u8>,
) -> Result<u64, TransactionError> {
    replay(
        fixture,
        clock_slot,
        market,
        fixture.base_vault.data.clone(),
        fixture.quote_vault.data.clone(),
    )
}

fn fixed_spread(bps: u64) -> [(&'static str, serde_json::Value); 4] {
    [
        ("spread", serde_json::json!(bps * 1_000)),
        ("tier_1_spread", serde_json::json!("0")),
        ("tier_2_spread", serde_json::json!("0")),
        ("tier_3_spread", serde_json::json!("0")),
    ]
}

fn priced_market(fixture: &HumidifiFixture, fair_value: u64) -> Vec<u8> {
    let priced = apply_raw(
        "humidifi-price",
        &fixture.market.data,
        &[("fair_value", serde_json::json!(fair_value.to_string()))],
        fixture.slot,
    );
    apply_raw(
        "humidifi-freshness",
        &priced,
        &[("last_update_slot", serde_json::json!(0))],
        fixture.slot,
    )
}

/// Every template writes only its own bytes on every schema-8 market, active or stopped, and every
/// vault is a token account its market owns, which is what the vault guidance relies on.
#[tokio::test]
async fn humidifi_templates_write_only_proven_bytes_on_every_market() {
    let keys: Vec<Pubkey> = ALL_MARKETS
        .iter()
        .flat_map(|(market, base_vault, quote_vault)| {
            [market, base_vault, quote_vault].map(|key| Pubkey::from_str_const(key))
        })
        .collect();
    let accounts = fetch(&keys).await;
    for ((address, base_vault, quote_vault), fetched) in ALL_MARKETS.iter().zip(accounts.chunks(3))
    {
        let market = Pubkey::from_str_const(address);
        let data = &fetched[0].data;
        assert_eq!(fetched[0].owner, PROGRAM, "{address}");
        assert_eq!(data.len(), MARKET_SIZE, "{address}");
        assert_eq!(&data[8..16], &LAYOUT_TAG, "{address} lost the layout tag");
        assert_eq!(word(data, 1720), 8, "{address} is no longer schema 8");
        assert!(
            word(data, 608) ^ SLOT_MASK < 1000,
            "{address} staleness limit must stay below the -1000 the guidance uses for a stale quote"
        );
        for (vault_offset, mint_offset, vault, account) in [
            (480, 416, base_vault, &fetched[1]),
            (448, 384, quote_vault, &fetched[2]),
        ] {
            assert_eq!(
                masked_pubkey(data, vault_offset).to_string(),
                *vault,
                "{address} vault at {vault_offset}"
            );
            assert_eq!(
                pubkey_at(&account.data, 0),
                masked_pubkey(data, mint_offset),
                "{address}: {vault} mint"
            );
            assert_eq!(
                pubkey_at(&account.data, 32),
                market,
                "{address}: {vault} must be owned by its market"
            );
        }

        for template in ["humidifi-price", "humidifi-freshness", "humidifi-spread"] {
            assert_eq!(
                apply_raw(template, data, &[], 0),
                *data,
                "{template} must round-trip {address}"
            );
        }
        let slot = (word(data, 616) ^ SLOT_MASK) + 1_000;
        let priced = apply_raw(
            "humidifi-price",
            data,
            &[("fair_value", serde_json::json!("58546795155816"))],
            slot,
        );
        assert_only_within(data, &priced, &[FAIR_VALUE], address);
        assert_eq!(word(&priced, 576) ^ FAIR_VALUE_MASK, 58_546_795_155_816);

        let fresh = apply_raw(
            "humidifi-freshness",
            data,
            &[
                ("last_update_slot", serde_json::Value::Null),
                ("max_staleness_slots", serde_json::json!(40)),
            ],
            slot,
        );
        assert_only_within(data, &fresh, &[MAX_STALENESS, LAST_UPDATE], address);
        assert_eq!(word(&fresh, 616) ^ SLOT_MASK, slot);
        assert_eq!(word(&fresh, 608) ^ SLOT_MASK, 40);

        let aged = apply_raw(
            "humidifi-freshness",
            data,
            &[("last_update_slot", serde_json::json!(-3))],
            slot,
        );
        assert_only_within(data, &aged, &[LAST_UPDATE], address);
        assert_eq!(word(&aged, 616) ^ SLOT_MASK, slot - 3);

        for (offset, mask) in TIER_SPREADS {
            assert!(
                (word(data, offset) ^ mask) >> 48 < 10_000,
                "{address}: tier spread at {offset} no longer decodes to basis points"
            );
        }
        let spread = apply_raw("humidifi-spread", data, &fixed_spread(25), slot);
        assert_only_within(data, &spread, &SPREAD_WORDS, address);
        assert_eq!(word(&spread, 800) ^ MAKER_SPREAD_MASK, 25_000);
        for (offset, mask) in TIER_SPREADS {
            assert_eq!(
                word(&spread, offset) ^ mask,
                0,
                "{address}: tier spread at {offset}"
            );
        }

        for vault in [&fetched[1].data, &fetched[2].data] {
            assert_eq!(
                apply_raw("humidifi-vault-balance", vault, &[], slot),
                *vault
            );
            let changed = apply_raw(
                "humidifi-vault-balance",
                vault,
                &[("amount", serde_json::json!(123u64))],
                slot,
            );
            assert_only_within(vault, &changed, &[AMOUNT], address);
            assert_eq!(amount(&changed), 123);
        }
    }
}

/// The featured addresses Studio and the README name unmask from the market's own fields, so a
/// replaced market fails here instead of silently targeting the wrong account.
#[tokio::test]
async fn humidifi_featured_markets_match_their_market_accounts() {
    for def in FEATURED_MARKETS {
        let a = fetch(&[
            def.market,
            def.base_vault,
            def.quote_vault,
            def.base_mint,
            def.quote_mint,
        ])
        .await;
        let data = &a[0].data;
        assert_eq!(a[0].owner, PROGRAM, "{}", def.pair);
        assert_eq!(data.len(), MARKET_SIZE, "{}", def.pair);
        assert_eq!(&data[8..16], &LAYOUT_TAG, "{}", def.pair);
        assert_eq!(word(data, 1720), 8, "{} is no longer schema 8", def.pair);
        for (offset, expected, what) in [
            (384, def.quote_mint, "quote mint"),
            (416, def.base_mint, "base mint"),
            (448, def.quote_vault, "quote vault"),
            (480, def.base_vault, "base vault"),
        ] {
            assert_eq!(masked_pubkey(data, offset), expected, "{} {what}", def.pair);
        }
        for (vault, mint, what) in [(&a[1], &a[3], "base"), (&a[2], &a[4], "quote")] {
            assert_eq!(
                vault.owner, mint.owner,
                "{} {what} vault token program",
                def.pair
            );
            assert_eq!(
                pubkey_at(&vault.data, 32),
                def.market,
                "{} {what} vault owner",
                def.pair
            );
        }
    }
}

#[tokio::test]
async fn humidifi_price_moves_the_executed_fill_on_the_fixture_markets() {
    for fixture in fixtures().await.iter() {
        let upstream = word(&fixture.market.data, 576) ^ FAIR_VALUE_MASK;
        let mut outputs = Vec::new();
        for fair_value in [upstream, upstream * 2] {
            let output = run(fixture, fixture.slot, priced_market(fixture, fair_value))
                .unwrap_or_else(|e| {
                    panic!(
                        "{} fair value {fair_value} must fill: {e:?}",
                        fixture.def.pair
                    )
                });
            let expected = expected_output(fair_value);
            assert!(
                output.abs_diff(expected) <= expected / 100,
                "{} fair value {fair_value}: output {output} is more than 1% from {expected}",
                fixture.def.pair
            );
            outputs.push(output);
        }
        let ratio = outputs[0] as f64 / outputs[1] as f64;
        assert!(
            (ratio - 2.0).abs() < 0.02,
            "{}: a doubled fair value must halve the base bought, got {ratio}",
            fixture.def.pair
        );
    }
}

#[tokio::test]
async fn humidifi_spread_widens_the_executed_fill_on_the_fixture_markets() {
    for fixture in fixtures().await.iter() {
        let upstream = word(&fixture.market.data, 576) ^ FAIR_VALUE_MASK;
        let priced = priced_market(fixture, upstream);
        let outputs: Vec<u64> = [0, 100]
            .into_iter()
            .map(|bps| {
                let market = apply_raw("humidifi-spread", &priced, &fixed_spread(bps), 0);
                run(fixture, fixture.slot, market).unwrap_or_else(|e| {
                    panic!("{} spread {bps} bps must fill: {e:?}", fixture.def.pair)
                })
            })
            .collect();
        let ratio = outputs[1] as f64 / outputs[0] as f64;
        assert!(
            (ratio - 1.0 / 1.01).abs() < 1e-4,
            "{}: a 100 bps spread must cut the base bought by 1/1.01, got {ratio}",
            fixture.def.pair
        );
    }
}

#[tokio::test]
async fn humidifi_price_scenario_materializes_through_surfnet() {
    let fixture = &fixtures().await[0];
    let (mut svm, _simnet_events_rx, _geyser_events_rx) = SurfnetSvm::default();
    svm.inner
        .set_account(fixture.def.market, fixture.market.clone())
        .expect("seed market");

    let mut scenario = Scenario::new(
        "HumidiFi SOL/USDC at 208".to_string(),
        "Reprice SOL to 208 USDC and keep the quote fresh".to_string(),
    );
    for (template_id, override_values) in [
        (
            "humidifi-price",
            values(&[("fair_value", serde_json::json!("58546795155816"))]),
        ),
        (
            "humidifi-freshness",
            values(&[("last_update_slot", serde_json::json!(0))]),
        ),
    ] {
        scenario.add_override(
            OverrideInstance::new(
                template_id.to_string(),
                0,
                AccountAddress::Pubkey(fixture.def.market.to_string()),
            )
            .with_values(override_values),
        );
    }
    let slot = fixture.slot + 500;
    svm.register_scenario(scenario, Some(slot))
        .expect("register the price scenario");
    svm.materialize_overrides_for_slot(&None, slot)
        .await
        .expect("materialize the price scenario");

    let market = svm
        .inner
        .get_account(&fixture.def.market)
        .unwrap()
        .unwrap()
        .data;
    assert_only_within(
        &fixture.market.data,
        &market,
        &[FAIR_VALUE, LAST_UPDATE],
        "scenario market",
    );
    assert_eq!(word(&market, 576) ^ FAIR_VALUE_MASK, 58_546_795_155_816);
    assert_eq!(word(&market, 616) ^ SLOT_MASK, slot);

    let output = run(fixture, slot, market).expect("the materialized quote must fill");
    let expected = expected_output(58_546_795_155_816);
    assert!(
        output.abs_diff(expected) <= expected / 100,
        "output {output} is more than 1% from {expected}"
    );
}

#[tokio::test]
async fn humidifi_freshness_boundary_is_real_program_behavior_on_the_fixture_markets() {
    for fixture in fixtures().await.iter() {
        let clock = fixture.slot + 100;
        for limit in [fixture.max_staleness, fixture.max_staleness + 8] {
            for age in [limit, limit + 1] {
                let staged = apply_raw(
                    "humidifi-freshness",
                    &fixture.market.data,
                    &[
                        ("last_update_slot", serde_json::json!(-(age as i64))),
                        ("max_staleness_slots", serde_json::json!(limit)),
                    ],
                    clock,
                );
                let result = run(fixture, clock, staged);
                let context = format!("{} limit {limit}, age {age}", fixture.def.pair);
                if age > limit {
                    assert_eq!(
                        result,
                        Err(TransactionError::InstructionError(
                            1,
                            InstructionError::Custom(STALE_QUOTE_ERROR)
                        )),
                        "{context}"
                    );
                } else {
                    assert!(
                        result.unwrap_or_else(|e| panic!("{context}: {e:?}")) > 0,
                        "{context}: a quote at the limit must fill"
                    );
                }
            }
        }
        let stale = apply_raw(
            "humidifi-freshness",
            &fixture.market.data,
            &[("last_update_slot", serde_json::json!(-1000))],
            clock,
        );
        assert_eq!(
            run(fixture, clock, stale),
            Err(TransactionError::InstructionError(
                1,
                InstructionError::Custom(STALE_QUOTE_ERROR)
            )),
            "{}: the documented stale offset must reject",
            fixture.def.pair
        );
    }
}

async fn materialized_vault(slot: u64, address: Pubkey, seeded: Account, balance: u64) -> Vec<u8> {
    let (mut svm, _simnet_events_rx, _geyser_events_rx) = SurfnetSvm::default();
    svm.inner.set_account(address, seeded).expect("seed vault");
    let mut scenario = Scenario::new(
        "HumidiFi vault inventory".to_string(),
        "Lower one HumidiFi vault".to_string(),
    );
    scenario.add_override(
        OverrideInstance::new(
            "humidifi-vault-balance".to_string(),
            0,
            AccountAddress::Pubkey(address.to_string()),
        )
        .with_values(values(&[("amount", serde_json::json!(balance))])),
    );
    svm.register_scenario(scenario, Some(slot))
        .expect("register the vault scenario");
    svm.materialize_overrides_for_slot(&None, slot)
        .await
        .expect("materialize the vault scenario");
    svm.inner.get_account(&address).unwrap().unwrap().data
}

#[tokio::test]
async fn humidifi_vault_scenario_drains_only_the_selected_side() {
    for fixture in fixtures().await.iter() {
        let context = fixture.def.pair;
        let baseline =
            run(fixture, fixture.slot, fixture.market.data.clone()).expect("baseline swap");
        let base_swap = |base_vault: Vec<u8>| {
            replay(
                fixture,
                fixture.slot,
                fixture.market.data.clone(),
                base_vault,
                fixture.quote_vault.data.clone(),
            )
        };

        let drained_base = materialized_vault(
            fixture.slot,
            fixture.def.base_vault,
            fixture.base_vault.clone(),
            0,
        )
        .await;
        assert_only_within(&fixture.base_vault.data, &drained_base, &[AMOUNT], context);
        assert_eq!(amount(&drained_base), 0);
        assert_eq!(
            base_swap(drained_base),
            Err(TransactionError::InstructionError(
                1,
                InstructionError::Custom(EMPTY_PAYOUT_ERROR)
            )),
            "{context}"
        );

        let thin_balance = amount(&fixture.base_vault.data) / 200;
        let thin_base = materialized_vault(
            fixture.slot,
            fixture.def.base_vault,
            fixture.base_vault.clone(),
            thin_balance,
        )
        .await;
        let thin_output = base_swap(thin_base).expect("a thin base vault still settles");
        assert!(
            thin_output < baseline,
            "{context}: thin inventory {thin_output} must quote below {baseline}"
        );

        // The quote vault only receives this direction's input.
        let drained_quote = materialized_vault(
            fixture.slot,
            fixture.def.quote_vault,
            fixture.quote_vault.clone(),
            0,
        )
        .await;
        assert_eq!(amount(&drained_quote), 0);
        assert_eq!(
            replay(
                fixture,
                fixture.slot,
                fixture.market.data.clone(),
                fixture.base_vault.data.clone(),
                drained_quote
            ),
            Ok(baseline),
            "{context}"
        );
    }
}
