use solana_clock::Slot;
use solana_pubkey::Pubkey;

pub mod bisonfi;
#[path = "phoenix-eternal/mod.rs"]
pub mod phoenix_eternal;
pub mod pump;

/// Runs the protocol code a raw-layout write needs for state its template's bytes cannot express,
/// chosen by the program that owns the account. Every other account is left as the template wrote it.
pub fn finish_raw_layout_write(
    owner: &Pubkey,
    template_id: &str,
    data: &mut [u8],
    slot: Slot,
    block_time_ms: u64,
) {
    if *owner == bisonfi::BISONFI_PROGRAM_ID {
        bisonfi::complete_publication(template_id, data, slot, block_time_ms);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A freshness write that `bisonfi-freshness` just published in slot 500.
    fn freshness_write() -> Vec<u8> {
        let mut data = vec![0u8; 2048];
        data[72..80].copy_from_slice(&500u64.to_le_bytes());
        data
    }

    #[test]
    fn a_bisonfi_account_gets_its_protocol_code() {
        let mut data = freshness_write();
        finish_raw_layout_write(
            &bisonfi::BISONFI_PROGRAM_ID,
            "bisonfi-freshness",
            &mut data,
            500,
            1_800_000_000_123,
        );
        assert_ne!(data, freshness_write());
    }

    #[test]
    fn any_other_program_keeps_what_its_template_wrote() {
        let mut data = freshness_write();
        finish_raw_layout_write(
            &Pubkey::new_unique(),
            "bisonfi-freshness",
            &mut data,
            500,
            1_800_000_000_123,
        );
        assert_eq!(data, freshness_write());
    }
}
