use phoenix_rise_accounts::{
    PhoenixAccountDecodeError,
    pda::{derive_permission_address, derive_spline_collection_address},
    perp_asset_map::PerpAssetMap,
    spline_collection::SplineCollection,
    trader::TraderHeader,
};
use solana_account::Account;
use solana_clock::Clock;
use solana_commitment_config::CommitmentConfig;
use solana_instruction::Instruction;
use solana_pubkey::Pubkey;

use super::state_builder::{
    Exchange, PHOENIX_GLOBAL_CONFIG, PHOENIX_PROGRAM_ID, hydrate, log_authority,
    phoenix_instruction, run_instructions,
};
use crate::{
    error::{SurfpoolError, SurfpoolResult},
    surfnet::{remote::SurfnetRemoteClient, svm::SurfnetSvm},
};

/// Crossed resting orders the uncross crank matches in one call.
const UNCROSS_MATCH_LIMIT: u64 = 64;

/// What a caller can name a market by, and the current values a relative change starts from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PhoenixMarket {
    pub symbol: String,
    pub orderbook: Pubkey,
    pub mark_ticks: u64,
    pub tick_size: u64,
    pub base_lot_decimals: i8,
    pub maintenance_risk_factor_bps: u16,
    pub backstop_risk_factor_bps: u16,
}

pub fn phoenix_markets(
    perp_asset_map: Pubkey,
    account: &Account,
) -> SurfpoolResult<Vec<PhoenixMarket>> {
    if account.owner != PHOENIX_PROGRAM_ID {
        return Err(SurfpoolError::invalid_account_owner(
            perp_asset_map,
            None::<PhoenixAccountDecodeError>,
        ));
    }
    let invalid = |error: PhoenixAccountDecodeError| {
        SurfpoolError::invalid_account_data(
            perp_asset_map,
            "Expected a valid Phoenix Eternal PerpAssetMap account",
            Some(error),
        )
    };
    let map = PerpAssetMap::try_from_account_bytes(&account.data).map_err(invalid)?;
    let mut markets = map
        .iter()
        .map(|entry| {
            entry.map(|entry| PhoenixMarket {
                symbol: entry.symbol.as_str().to_string(),
                orderbook: Pubkey::new_from_array(
                    entry.metadata.static_market_params().market_account,
                ),
                mark_ticks: entry
                    .metadata
                    .oracle_price()
                    .mark_price
                    .price
                    .ticks
                    .as_inner(),
                tick_size: entry.metadata.static_market_params().tick_size.as_inner(),
                base_lot_decimals: entry.metadata.static_market_params().base_lot_decimals,
                maintenance_risk_factor_bps: entry.metadata.risk_params().risk_factors[0],
                backstop_risk_factor_bps: entry.metadata.risk_params().risk_factors[1],
            })
        })
        .collect::<Result<Vec<_>, _>>()
        .map_err(invalid)?;
    markets.sort_unstable_by(|left, right| left.symbol.cmp(&right.symbol));

    Ok(markets)
}

/// The orderbook and spline collection of every listed market whose asset id is in `assets`.
pub fn market_books(
    perp_asset_map: &Pubkey,
    data: &[u8],
    assets: &[u64],
) -> SurfpoolResult<Vec<(Pubkey, Pubkey)>> {
    let invalid = |e| {
        SurfpoolError::invalid_account_data(
            perp_asset_map,
            "Expected the Phoenix Eternal PerpAssetMap",
            Some(e),
        )
    };
    let map = PerpAssetMap::try_from_account_bytes(data).map_err(invalid)?;
    let mut books = Vec::new();
    for entry in map.iter() {
        let entry = entry.map_err(invalid)?;
        let params = entry.metadata.static_market_params();
        if assets.contains(&u64::from(params.asset_id())) {
            let orderbook = Pubkey::new_from_array(params.market_account);
            books.push((
                orderbook,
                derive_spline_collection_address(&PHOENIX_PROGRAM_ID, &orderbook),
            ));
        }
    }
    Ok(books)
}

/// One market as the PerpAssetMap describes it.
pub struct Market {
    pub symbol: String,
    pub asset_id: u64,
    pub orderbook: Pubkey,
    pub splines: Pubkey,
    pub tick_size: u64,
    pub base_lot_decimals: i8,
    pub mark_ticks: u64,
    /// The most base lots one liquidation may close.
    pub max_liquidation_size: u64,
    /// Maintenance, backstop and high-risk factors, in basis points of the initial margin.
    pub risk_factors: [u16; 3],
    /// The keys that report this market's oracle prices.
    pub oracle_keys: Vec<Pubkey>,
    pub latest_oracle_timestamp: u64,
}

impl Market {
    pub fn find(perp_asset_map: &Pubkey, data: &[u8], symbol: &str) -> SurfpoolResult<Self> {
        let invalid = |e| {
            SurfpoolError::invalid_account_data(
                perp_asset_map,
                "Expected the Phoenix Eternal PerpAssetMap",
                Some(e),
            )
        };
        let map = PerpAssetMap::try_from_account_bytes(data).map_err(invalid)?;
        let entry = map
            .find_by_symbol(symbol)
            .map_err(invalid)?
            .ok_or_else(|| SurfpoolError::internal(format!("Phoenix lists no market {symbol}")))?;
        let params = entry.metadata.static_market_params();
        let orderbook = Pubkey::new_from_array(params.market_account);
        let mark = entry.metadata.oracle_price().mark_price;
        let oracle_keys: Vec<Pubkey> = mark
            .oracle_data
            .iter()
            .map(|oracle| Pubkey::new_from_array(oracle.oracle_pubkey))
            .filter(|key| *key != Pubkey::default())
            .collect();
        if oracle_keys.is_empty() {
            return Err(SurfpoolError::internal(format!(
                "Phoenix market {symbol} has no oracle that reports its price"
            )));
        }
        Ok(Self {
            symbol: symbol.to_string(),
            asset_id: u64::from(params.asset_id()),
            orderbook,
            splines: derive_spline_collection_address(&PHOENIX_PROGRAM_ID, &orderbook),
            tick_size: params.tick_size.as_inner(),
            base_lot_decimals: params.base_lot_decimals,
            mark_ticks: mark.price.ticks.as_inner(),
            max_liquidation_size: entry.metadata.risk_params().max_liquidation_size.as_inner(),
            risk_factors: entry.metadata.risk_params().risk_factors,
            oracle_keys,
            latest_oracle_timestamp: mark
                .oracle_last_updated_timestamps
                .iter()
                .copied()
                .max()
                .unwrap_or_default(),
        })
    }

    /// The oracle price for `ticks`, as a value and a decimal exponent: USD per base unit is
    /// ticks × tick size × 10^(base lot decimals − 6).
    pub fn oracle_price(&self, ticks: u64) -> SurfpoolResult<(u64, u8)> {
        let overflow =
            || SurfpoolError::internal(format!("{ticks} ticks overflow {}'s price", self.symbol));
        let value = ticks.checked_mul(self.tick_size).ok_or_else(overflow)?;
        let decimals = 6 - i32::from(self.base_lot_decimals);
        if decimals >= 0 {
            Ok((value, decimals as u8))
        } else {
            let scale = 10_u64
                .checked_pow(decimals.unsigned_abs())
                .ok_or_else(overflow)?;
            Ok((value.checked_mul(scale).ok_or_else(overflow)?, 0))
        }
    }
}

/// A market with everything a move needs in the local VM: its accounts, the keys that report its
/// oracle prices with their permissions, and the makers quoting it.
pub struct MarketContext {
    pub exchange: Exchange,
    pub market: Market,
    oracle_permissions: Vec<Pubkey>,
    /// Each enabled maker's Trader, the authority that signs for it, its last price sequence and
    /// the slot its updates report.
    makers: Vec<(Pubkey, Pubkey, u64, u64)>,
    next_timestamp: u64,
}

impl MarketContext {
    pub async fn load(
        svm: &mut SurfnetSvm,
        remote_ctx: &Option<(SurfnetRemoteClient, CommitmentConfig)>,
        symbol: &str,
    ) -> SurfpoolResult<Self> {
        let exchange = Exchange::load(svm, remote_ctx).await?;
        hydrate(
            svm,
            remote_ctx,
            &[
                exchange.perp_asset_map,
                exchange.global_trader_index,
                exchange.active_trader_buffer,
            ],
        )
        .await?;
        let map = local_account(svm, &exchange.perp_asset_map)?;
        let market = Market::find(&exchange.perp_asset_map, &map.data, symbol)?;
        hydrate(svm, remote_ctx, &[market.orderbook, market.splines]).await?;

        let splines = local_account(svm, &market.splines)?;
        let collection = SplineCollection::try_from_account_bytes(&splines.data).map_err(|e| {
            SurfpoolError::invalid_account_data(
                market.splines,
                "Expected a Phoenix spline collection",
                Some(e),
            )
        })?;
        // A spline refuses an update slot below its last one, and a running surfnet's slot trails
        // the mainnet slot that last update came from.
        let slot = svm.inner.get_sysvar::<Clock>().slot;
        let enabled: Vec<(Pubkey, u64, u64)> = collection
            .splines()
            .filter_map(Result::ok)
            .filter(|spline| spline.is_enabled())
            .map(|spline| {
                (
                    Pubkey::new_from_array(spline.trader()),
                    spline.user_price_sequence_number(),
                    spline.user_update_slot().max(slot),
                )
            })
            .collect();
        let oracle_authority = Pubkey::new_from_array(exchange.config.oracle_authority());
        let oracle_permissions: Vec<Pubkey> = market
            .oracle_keys
            .iter()
            .map(|key| derive_permission_address(&PHOENIX_PROGRAM_ID, &oracle_authority, key))
            .collect();
        let mut dependencies: Vec<Pubkey> = enabled.iter().map(|(trader, ..)| *trader).collect();
        dependencies.extend(&oracle_permissions);
        hydrate(svm, remote_ctx, &dependencies).await?;

        let mut makers = Vec::with_capacity(enabled.len());
        for (trader, sequence, update_slot) in enabled {
            let account = local_account(svm, &trader)?;
            let header = TraderHeader::try_read_from_account_bytes(&account.data).map_err(|e| {
                SurfpoolError::invalid_account_data(trader, "Expected a Phoenix Trader", Some(e))
            })?;
            makers.push((
                trader,
                Pubkey::new_from_array(header.authority),
                sequence,
                update_slot,
            ));
        }
        // An oracle refuses a report older than its last one.
        let clock_ms = (svm.inner.get_sysvar::<Clock>().unix_timestamp.max(0) as u64) * 1000;
        Ok(Self {
            next_timestamp: clock_ms.max(market.latest_oracle_timestamp) + 1,
            exchange,
            market,
            oracle_permissions,
            makers,
        })
    }

    /// Every oracle key reporting `ticks`, which moves the mark there. Each call reports later
    /// than the previous one.
    pub fn oracle_reports(&mut self, ticks: u64) -> SurfpoolResult<Vec<Instruction>> {
        checked_ticks(ticks)?;
        let (value, expo) = self.market.oracle_price(ticks)?;
        let price = serde_json::json!({ "value": value, "expo": expo });
        let mut symbol_bytes = [0_u8; 16];
        let symbol = self.market.symbol.as_bytes();
        let len = symbol.len().min(symbol_bytes.len());
        symbol_bytes[..len].copy_from_slice(&symbol[..len]);
        let timestamp = self.next_timestamp;
        self.next_timestamp += 1;
        self.market
            .oracle_keys
            .iter()
            .zip(&self.oracle_permissions)
            .map(|(key, permission)| {
                phoenix_instruction(
                    "UpdateOraclePricesWithOrdering",
                    &[
                        ("phoenixProgram", PHOENIX_PROGRAM_ID),
                        ("phoenixLogAuthority", log_authority()),
                        ("globalConfiguration", PHOENIX_GLOBAL_CONFIG),
                        ("authority", *key),
                        ("permissionAccount", *permission),
                        ("perpAssetMap", self.exchange.perp_asset_map),
                    ],
                    &serde_json::json!({
                        "params": {
                            "updateTimestamp": timestamp,
                            "updates": [{
                                "perpAssetId": { "symbolBytes": symbol_bytes },
                                "newExchangePerpPrice": price,
                                "newExchangeSpotPrice": price,
                            }],
                            "shouldResetTimestamp": false,
                        }
                    }),
                )
            })
            .collect()
    }

    /// Every enabled maker re-centring its spline on `ticks`.
    pub fn spline_moves(&mut self, ticks: u64) -> SurfpoolResult<Vec<Instruction>> {
        checked_ticks(ticks)?;
        let mut instructions = Vec::with_capacity(self.makers.len());
        for (trader, authority, sequence, update_slot) in &mut self.makers {
            *sequence += 1;
            let client_order_id = [0_u8; 16];
            instructions.push(phoenix_instruction(
                "UpdateSplinePrice",
                &[
                    ("phoenixProgram", PHOENIX_PROGRAM_ID),
                    ("phoenixLogAuthority", log_authority()),
                    ("signerAccount", *authority),
                    ("traderAccount", *trader),
                    ("splineAccount", self.market.splines),
                    ("orderbook", self.market.orderbook),
                ],
                &serde_json::json!({
                    "params": {
                        "newMidPrice": ticks,
                        "userUpdateSlot": *update_slot,
                        "refreshRegions": true,
                        "userSequenceNumber": *sequence,
                        "clientOrderId": client_order_id,
                        "overrideSequenceNumber": false,
                    }
                }),
            )?);
        }
        Ok(instructions)
    }

    /// The crank that matches resting orders a move left crossed.
    pub fn uncross(&self) -> SurfpoolResult<Instruction> {
        phoenix_instruction(
            "UncrossCrank",
            &[
                ("phoenixProgram", PHOENIX_PROGRAM_ID),
                ("phoenixLogAuthority", log_authority()),
                ("globalConfiguration", PHOENIX_GLOBAL_CONFIG),
                ("perpAssetMap", self.exchange.perp_asset_map),
                ("globalTraderIndex", self.exchange.global_trader_index),
                ("activeTraderBuffer", self.exchange.active_trader_buffer),
                ("orderbook", self.market.orderbook),
                ("splines", self.market.splines),
            ],
            &serde_json::json!({ "params": { "matchLimit": UNCROSS_MATCH_LIMIT } }),
        )
    }

    /// The whole move to `ticks`: oracle reports, spline moves and the uncross crank.
    pub fn move_to(&mut self, ticks: u64) -> SurfpoolResult<Vec<Instruction>> {
        let mut instructions = self.oracle_reports(ticks)?;
        instructions.extend(self.spline_moves(ticks)?);
        instructions.push(self.uncross()?);
        Ok(instructions)
    }
}

/// The writes that move one market to `target_ticks` the way mainnet moves it: every oracle key
/// reports the new price, every active maker re-centres its spline on it, and the crank matches
/// resting orders the move left crossed. Prices come out of the program itself, so the mark,
/// the book and the oracle readings agree.
pub async fn move_market(
    svm: &mut SurfnetSvm,
    remote_ctx: &Option<(SurfnetRemoteClient, CommitmentConfig)>,
    symbol: &str,
    target_ticks: u64,
) -> SurfpoolResult<Vec<(Pubkey, Account)>> {
    let mut context = MarketContext::load(svm, remote_ctx, symbol).await?;
    let instructions = context.move_to(target_ticks)?;
    run_instructions(svm, &instructions)
}

fn checked_ticks(ticks: u64) -> SurfpoolResult<()> {
    if ticks == 0 || ticks > u64::from(u32::MAX) {
        return Err(SurfpoolError::internal(format!(
            "{ticks} ticks are outside Phoenix's price range 1..={}",
            u32::MAX
        )));
    }
    Ok(())
}

pub(crate) fn local_account(svm: &SurfnetSvm, address: &Pubkey) -> SurfpoolResult<Account> {
    svm.inner
        .get_account(address)?
        .ok_or_else(|| SurfpoolError::internal(format!("{address} is missing locally")))
}

#[cfg(test)]
pub(crate) mod tests {
    use base64::{Engine, prelude::BASE64_STANDARD};

    use super::{super::state_builder::PHOENIX_PERP_ASSET_MAP, *};

    const PERP_ASSET_MAP_LEN: usize = 1_622_064;
    const SOL_PERP_ASSET_MAP_PREFIX_B64: &str = "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";

    pub(crate) fn perp_asset_map_fixture() -> Vec<u8> {
        let prefix = BASE64_STANDARD
            .decode(SOL_PERP_ASSET_MAP_PREFIX_B64)
            .unwrap();
        let mut data = vec![0_u8; PERP_ASSET_MAP_LEN];
        data[..prefix.len()].copy_from_slice(&prefix);
        data[24..26].copy_from_slice(&1_u16.to_le_bytes());
        data[32..36].copy_from_slice(&1_u32.to_le_bytes());
        data[36..40].copy_from_slice(&0_u32.to_le_bytes());
        data
    }

    pub(crate) fn perp_asset_map_account() -> Account {
        Account {
            lamports: 1,
            data: perp_asset_map_fixture(),
            owner: PHOENIX_PROGRAM_ID,
            executable: false,
            rent_epoch: 0,
        }
    }

    fn market(tick_size: u64, base_lot_decimals: i8) -> Market {
        Market {
            symbol: "SOL".to_string(),
            asset_id: 0,
            orderbook: Pubkey::new_unique(),
            splines: Pubkey::new_unique(),
            tick_size,
            base_lot_decimals,
            mark_ticks: 1,
            max_liquidation_size: 1,
            risk_factors: [5_000, 2_000, 1_000],
            oracle_keys: vec![Pubkey::new_unique()],
            latest_oracle_timestamp: 0,
        }
    }

    #[test]
    fn converts_ticks_to_the_oracle_price() {
        // SOL: tick size 100, base lot decimals 2, so 10730 ticks are 107.30 USD.
        assert_eq!(market(100, 2).oracle_price(10_730).unwrap(), (1_073_000, 4));
        assert_eq!(market(1, -3).oracle_price(5).unwrap(), (5, 9));
        assert_eq!(market(3, 8).oracle_price(7).unwrap(), (2_100, 0));
        assert!(market(u64::MAX, 2).oracle_price(2).is_err());
    }

    #[test]
    fn lists_the_markets_a_caller_can_name() {
        let markets = phoenix_markets(PHOENIX_PERP_ASSET_MAP, &perp_asset_map_account()).unwrap();
        assert_eq!(
            markets,
            vec![PhoenixMarket {
                symbol: "SOL".to_string(),
                orderbook: Pubkey::from_str_const("71Si24E4uc3oCaPbPZTozC1ptSNNqygjjebxSmErSsC2"),
                mark_ticks: markets[0].mark_ticks,
                tick_size: 100,
                base_lot_decimals: 2,
                maintenance_risk_factor_bps: 5_000,
                backstop_risk_factor_bps: 2_000,
            }]
        );
        let foreign = Account {
            owner: Pubkey::new_unique(),
            ..perp_asset_map_account()
        };
        assert!(phoenix_markets(PHOENIX_PERP_ASSET_MAP, &foreign).is_err());
    }
}
