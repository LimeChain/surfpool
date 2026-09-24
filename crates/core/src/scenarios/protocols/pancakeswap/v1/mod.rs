use solana_pubkey::Pubkey;

use crate::scenarios::protocols::raydium::v3::price_shock_builder::ClmmProgram;

pub const PANCAKESWAP: ClmmProgram = ClmmProgram {
    program_id: Pubkey::from_str_const("HpNfyc2Saw7RKkQd8nEL4khUcuPhQ7WwY1B2qjx8jxFq"),
    pool_state_template: "pancakeswap-clmm-pool-state",
};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scenarios::protocols::raydium::v3::price_shock_builder::tick_array_address;

    #[test]
    fn pancakeswap_derives_a_live_tick_array_address() {
        let pool = Pubkey::from_str_const("14UxBHXXaYhqbmwFCk9gywJcj9semTtdegqBnWNVGFxw");
        assert_eq!(
            tick_array_address(&PANCAKESWAP.program_id, &pool, -23400),
            Pubkey::from_str_const("G7PLD8dqNb7uyiJAs9tq9tF4R4WeN7nwoKQUMffzPusb"),
        );
    }
}
