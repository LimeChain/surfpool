//! BisonFi state a raw-layout template cannot write as plain bytes. Scenario execution calls into
//! this after a BisonFi template's own write, as it calls Phoenix's builder for Phoenix accounts.
//!
//! The freshness template writes the slot a quote was published in (byte 72). A real publication
//! also leaves two other fields the program reads, which a slot alone cannot set:
//!
//! - The nanosecond time the maker published (byte 88). The program grades a quote's age to the
//!   millisecond as the market tick's time minus this: up to 150 ms it prices the full book, older a
//!   thinner one. On a surfnet the tick follows the clock while this keeps the time the pool was
//!   fetched with, so a quote refreshed by slot alone would still read as aged.
//! - The slot the program last rebuilt the book in (byte 688). On the first swap of a slot the quote
//!   was published in, the program refills the book from its source - but only while this is in the
//!   past. A pool fetched while the surfnet is behind mainnet carries mainnet's later slot here, and
//!   consumed depth would stay consumed until the surfnet caught up.

use solana_clock::Slot;
use solana_pubkey::Pubkey;

pub const BISONFI_PROGRAM_ID: Pubkey =
    Pubkey::from_str_const("BiSoNHVpsVZW2F7rx2eQ59yQwKxzU5NvBcmKshCSUypi");

const FRESHNESS_TEMPLATE: &str = "bisonfi-freshness";
const LAST_UPDATE_SLOT: usize = 72;
const PUBLICATION_TIME: usize = 88;
const LAST_REBUILD_SLOT: usize = 688;

fn read_u64(data: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes(data[offset..offset + 8].try_into().expect("8 bytes"))
}

/// Completes what `bisonfi-freshness` publishes when it publishes the quote in the slot it
/// materializes at: stamps the publication time from the surfnet clock, and moves a last rebuild
/// slot that is not in the past back by one, so the next swap refills the book as it would after a
/// real publication. A quote the template ages (a negative slot offset) is left as written: its slot
/// age already grades it.
pub fn complete_publication(template_id: &str, data: &mut [u8], slot: Slot, block_time_ms: u64) {
    if template_id != FRESHNESS_TEMPLATE || data.len() < LAST_REBUILD_SLOT + 8 {
        return;
    }
    if read_u64(data, LAST_UPDATE_SLOT) != slot {
        return;
    }
    let nanos = block_time_ms.saturating_mul(1_000_000);
    data[PUBLICATION_TIME..PUBLICATION_TIME + 8].copy_from_slice(&nanos.to_le_bytes());
    if read_u64(data, LAST_REBUILD_SLOT) >= slot {
        let previous = slot.saturating_sub(1);
        data[LAST_REBUILD_SLOT..LAST_REBUILD_SLOT + 8].copy_from_slice(&previous.to_le_bytes());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BLOCK_TIME_MS: u64 = 1_800_000_000_123;

    fn pool(last_update_slot: u64, publication_ns: u64, last_rebuild_slot: u64) -> Vec<u8> {
        let mut data = vec![0u8; 2048];
        data[72..80].copy_from_slice(&last_update_slot.to_le_bytes());
        data[88..96].copy_from_slice(&publication_ns.to_le_bytes());
        data[688..696].copy_from_slice(&last_rebuild_slot.to_le_bytes());
        data
    }

    #[test]
    fn stamps_the_time_of_a_quote_published_this_slot() {
        let mut data = pool(500, 7, 499);
        complete_publication(FRESHNESS_TEMPLATE, &mut data, 500, BLOCK_TIME_MS);
        assert_eq!(read_u64(&data, 88), BLOCK_TIME_MS * 1_000_000);
    }

    #[test]
    fn moves_a_rebuild_slot_from_mainnets_future_into_the_past() {
        let mut data = pool(500, 7, 530);
        complete_publication(FRESHNESS_TEMPLATE, &mut data, 500, BLOCK_TIME_MS);
        assert_eq!(read_u64(&data, 688), 499);
    }

    #[test]
    fn leaves_a_rebuild_slot_already_in_the_past() {
        let mut data = pool(500, 7, 420);
        complete_publication(FRESHNESS_TEMPLATE, &mut data, 500, BLOCK_TIME_MS);
        assert_eq!(read_u64(&data, 688), 420);
    }

    #[test]
    fn leaves_a_quote_the_template_aged() {
        let before = pool(498, 7, 530);
        let mut after = before.clone();
        complete_publication(FRESHNESS_TEMPLATE, &mut after, 500, BLOCK_TIME_MS);
        assert_eq!(after, before);
    }

    #[test]
    fn ignores_the_other_templates() {
        for template in ["bisonfi-fair-value", "bisonfi-depth", "bisonfi-spread"] {
            let before = pool(500, 7, 530);
            let mut after = before.clone();
            complete_publication(template, &mut after, 500, BLOCK_TIME_MS);
            assert_eq!(after, before, "{template} must not complete a publication");
        }
    }

    #[test]
    fn writes_nothing_but_the_two_fields() {
        let before = pool(500, 7, 530);
        let mut after = before.clone();
        complete_publication(FRESHNESS_TEMPLATE, &mut after, 500, BLOCK_TIME_MS);
        let changed: Vec<usize> = (0..before.len())
            .filter(|i| before[*i] != after[*i])
            .collect();
        assert!(
            changed
                .iter()
                .all(|i| (88..96).contains(i) || (688..696).contains(i)),
            "{changed:?}"
        );
    }

    #[test]
    fn ignores_an_account_too_short_to_hold_them() {
        let mut short = vec![0u8; 600];
        complete_publication(FRESHNESS_TEMPLATE, &mut short, 0, BLOCK_TIME_MS);
        assert_eq!(short, vec![0u8; 600]);
    }
}
