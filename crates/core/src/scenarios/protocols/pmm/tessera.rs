use surfpool_types::{RawEncoding, RawLayoutField, RawLayoutTemplate};

pub const TESSERA_MARKET_PRICE_TEMPLATE_ID: &str = "tessera-market-price";

const PUBLISH_SLOT_LEAD: i64 = -1;

pub fn market_price_template() -> RawLayoutTemplate {
    RawLayoutTemplate {
        id: TESSERA_MARKET_PRICE_TEMPLATE_ID.to_string(),
        name: "Tessera WSOL/USDC market price".to_string(),
        protocol: "Tessera".to_string(),
        base_account: None,
        fields: vec![
            RawLayoutField {
                offset: 128,
                encoding: RawEncoding::U64FixedPoint {
                    from_value: "price".to_string(),
                    scale: 1e12,
                },
            },
            RawLayoutField {
                offset: 144,
                encoding: RawEncoding::U64Reciprocal {
                    from_value: "price".to_string(),
                    scale: 1e18,
                },
            },
            RawLayoutField {
                offset: 120,
                encoding: RawEncoding::Slot {
                    lead: PUBLISH_SLOT_LEAD,
                },
            },
        ],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn template_is_well_formed() {
        let t = market_price_template();
        assert_eq!(t.id, TESSERA_MARKET_PRICE_TEMPLATE_ID);
        assert!(t.base_account.is_none(), "should fork the live book, not embed one");
        assert_eq!(t.fields.len(), 3);
    }
}
