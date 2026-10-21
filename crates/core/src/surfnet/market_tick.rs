//! The market tick: a clock account block builders publish for prop AMMs to read.
//!
//! Block builders (Harmonic and, by its key, Jito's BAM) rewrite this account several times per slot
//! with the slot and a nanosecond timestamp, signed with their own key, which the account also
//! records. Prop AMMs read it for a clock finer than `Clock::unix_timestamp`'s whole seconds, and
//! check it against the Clock before trusting it. BisonFi requires it on every swap since its
//! 2026-10-14 upgrade - without it a swap pays out zero, and a tick that disagrees with the Clock
//! falls back to a thinner book. Tessera requires it to be present and validates it, though no live
//! market consults its contents yet.
//!
//! Nothing writes it in a surfnet, so Surfpool restamps it whenever it moves the Clock and when it
//! first hydrates it from upstream.

use solana_account::Account;
use solana_clock::Slot;
use solana_pubkey::Pubkey;

pub const MARKET_TICK: Pubkey =
    Pubkey::from_str_const("4cG31VNF9TzFinNc7BmnjhFvGjxkY3sCETVMtMgbrhPs");
pub const MARKET_TICK_PROGRAM: Pubkey =
    Pubkey::from_str_const("tickUcsEQegChaAuo9VYQQztB4ZGApY6ZT4FkULWY6N");

const MARKET_TICK_DISCRIMINATOR: &[u8; 8] = b"MRKTKV01";
const MARKET_TICK_LEN: usize = 88;
const MARKET_TICK_SLOT: usize = 40;
/// Two nanosecond timestamps. The program reads the second; the first is kept equal so the account
/// still looks like one the builder wrote.
const MARKET_TICK_TIMESTAMPS: [usize; 2] = [48, 56];

/// Returns the tick at [`MARKET_TICK`] restamped to `slot` and `block_time_ms`, or `None` when the
/// account held there is not a market tick. The publisher identity at bytes 8..40 is left alone:
/// programs only accept publishers on their own allowlist, and the fetched account already carries
/// one of them.
pub fn stamp_market_tick(account: &Account, slot: Slot, block_time_ms: u64) -> Option<Account> {
    if account.owner != MARKET_TICK_PROGRAM
        || account.data.len() != MARKET_TICK_LEN
        || !account.data.starts_with(MARKET_TICK_DISCRIMINATOR)
    {
        return None;
    }
    let nanos = block_time_ms.saturating_mul(1_000_000);
    let mut stamped = account.clone();
    stamped.data[MARKET_TICK_SLOT..MARKET_TICK_SLOT + 8].copy_from_slice(&slot.to_le_bytes());
    for offset in MARKET_TICK_TIMESTAMPS {
        stamped.data[offset..offset + 8].copy_from_slice(&nanos.to_le_bytes());
    }
    Some(stamped)
}

/// The market tick as captured from mainnet on 2026-10-20, for tests that need one offline.
#[cfg(test)]
pub(crate) fn captured_market_tick() -> Account {
    const CAPTURED: &str = "4d524b544b563031f6676f7ce1cbc79fcb286c02743f551477b6a840e8caeb98537fada2450ce50f3ff95d1b000000008e6657617c2be0186d634c6a7c2be018030000000000000080f0fa02000000002bf2fa0200000000";
    let data = (0..CAPTURED.len() / 2)
        .map(|i| u8::from_str_radix(&CAPTURED[2 * i..2 * i + 2], 16).unwrap())
        .collect();
    Account {
        lamports: 1_503_360,
        data,
        owner: MARKET_TICK_PROGRAM,
        executable: false,
        rent_epoch: 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn live_tick() -> Account {
        captured_market_tick()
    }

    fn u64_at(data: &[u8], offset: usize) -> u64 {
        u64::from_le_bytes(data[offset..offset + 8].try_into().unwrap())
    }

    #[test]
    fn stamps_the_slot_and_both_timestamps() {
        let tick = live_tick();
        let stamped = stamp_market_tick(&tick, 500, 1_800_000_000_123).unwrap();

        assert_eq!(u64_at(&stamped.data, 40), 500);
        assert_eq!(u64_at(&stamped.data, 48), 1_800_000_000_123_000_000);
        assert_eq!(u64_at(&stamped.data, 56), 1_800_000_000_123_000_000);
    }

    #[test]
    fn leaves_every_other_byte_alone() {
        let tick = live_tick();
        let stamped = stamp_market_tick(&tick, 500, 1_800_000_000_123).unwrap();

        assert_eq!(
            stamped.data[..40],
            tick.data[..40],
            "publisher identity must survive"
        );
        assert_eq!(stamped.data[64..], tick.data[64..]);
        assert_eq!(stamped.owner, tick.owner);
        assert_eq!(stamped.lamports, tick.lamports);
    }

    /// The program rounds the tick to the nearest second and allows one second of disagreement with
    /// `Clock::unix_timestamp`, which Surfpool derives from the same milliseconds by truncation.
    #[test]
    fn the_stamp_satisfies_the_programs_clock_check() {
        let tick = live_tick();
        for block_time_ms in [
            1_800_000_000_000u64,
            1_800_000_000_499,
            1_800_000_000_500,
            1_800_000_000_999,
        ] {
            let stamped = stamp_market_tick(&tick, 1, block_time_ms).unwrap();
            let rounded = (u64_at(&stamped.data, 56) + 500_000_000) / 1_000_000_000;
            let clock_seconds = block_time_ms / 1_000;
            assert!(rounded.abs_diff(clock_seconds) <= 1, "{block_time_ms} ms");
        }
    }

    #[test]
    fn ignores_accounts_that_are_not_the_tick() {
        let tick = live_tick();
        let mut foreign = tick.clone();
        foreign.owner = Pubkey::new_unique();
        assert!(stamp_market_tick(&foreign, 1, 1).is_none());

        let mut short = tick.clone();
        short.data.truncate(64);
        assert!(stamp_market_tick(&short, 1, 1).is_none());

        let mut renamed = tick;
        renamed.data[0] ^= 0xff;
        assert!(stamp_market_tick(&renamed, 1, 1).is_none());
    }
}
