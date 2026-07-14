pub mod tessera;

use surfpool_types::RawLayoutTemplate;

pub use tessera::TESSERA_MARKET_PRICE_TEMPLATE_ID;

pub fn builtin_raw_templates() -> Vec<RawLayoutTemplate> {
    vec![tessera::market_price_template()]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_set_contains_tessera() {
        assert!(builtin_raw_templates()
            .iter()
            .any(|t| t.id == TESSERA_MARKET_PRICE_TEMPLATE_ID));
    }
}
