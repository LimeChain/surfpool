use std::{collections::HashSet, ops::Range};

use bytemuck::{Pod, Zeroable};
use phoenix_rise_accounts::{PhoenixAccount, multi_arena::MultiArenaHeader, trader::TraderHeader};
use solana_account::Account;
use solana_clock::Clock;
use solana_commitment_config::CommitmentConfig;
use solana_instruction::{AccountMeta, Instruction};
use solana_program_pack::Pack;
use solana_pubkey::Pubkey;
use spl_associated_token_account_interface::{
    address::get_associated_token_address_with_program_id,
    instruction::create_associated_token_account_idempotent,
};
use spl_token_interface::state::Mint;

use super::{
    market::{Market, MarketContext, explain_move_failure, phoenix_markets},
    state_builder::{
        Exchange, PHOENIX_GLOBAL_CONFIG, PHOENIX_PROGRAM_ID, Sandbox, hydrate, keep_markets_usable,
        local_account, log_authority, phoenix_instruction, run_instructions,
    },
};
use crate::{
    error::{SurfpoolError, SurfpoolResult},
    surfnet::{
        remote::SurfnetRemoteClient,
        svm::{SurfnetSvm, SurfnetSvmConfig},
    },
};

/// Which way an order trades: a bid buys, an ask sells.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Side {
    Bid,
    Ask,
}

impl Side {
    fn idl_name(self) -> &'static str {
        match self {
            Side::Bid => "Bid",
            Side::Ask => "Ask",
        }
    }
}

/// One trader and the wallet that signs for it.
pub struct TraderContext {
    pub exchange: Exchange,
    pub trader: Pubkey,
    pub authority: Pubkey,
}

impl TraderContext {
    pub async fn load(
        svm: &mut SurfnetSvm,
        remote_ctx: &Option<(SurfnetRemoteClient, CommitmentConfig)>,
        trader: Pubkey,
    ) -> SurfpoolResult<Self> {
        let exchange = Exchange::load(svm, remote_ctx).await?;
        hydrate(
            svm,
            remote_ctx,
            &[
                trader,
                exchange.perp_asset_map,
                exchange.global_trader_index,
                exchange.active_trader_buffer,
            ],
        )
        .await?;
        let account = local_account(svm, &trader)?;
        let header = TraderHeader::try_read_from_account_bytes(&account.data).map_err(|e| {
            SurfpoolError::invalid_account_data(trader, "Expected a Phoenix Trader", Some(e))
        })?;
        let authority = Pubkey::new_from_array(header.authority);
        hydrate(svm, remote_ctx, &[authority]).await?;
        Ok(Self {
            exchange,
            trader,
            authority,
        })
    }
}

/// The writes of a market order the trader sends itself: `base_lots` on `side`, filled against
/// the book at whatever price it offers.
pub async fn place_market_order(
    svm: &mut SurfnetSvm,
    remote_ctx: &Option<(SurfnetRemoteClient, CommitmentConfig)>,
    trader: Pubkey,
    symbol: &str,
    side: Side,
    base_lots: u64,
) -> SurfpoolResult<Vec<(Pubkey, Account)>> {
    if base_lots == 0 {
        return Err(SurfpoolError::internal(
            "a market order needs at least 1 base lot",
        ));
    }
    let context = TraderContext::load(svm, remote_ctx, trader).await?;
    let market = MarketContext::load(svm, remote_ctx, symbol).await?;
    let client_order_id = [0_u8; 16];
    let order = phoenix_instruction(
        "PlaceMarketOrder",
        &[
            ("phoenixProgram", PHOENIX_PROGRAM_ID),
            ("phoenixLogAuthority", log_authority()),
            ("globalConfiguration", PHOENIX_GLOBAL_CONFIG),
            ("traderWallet", context.authority),
            ("traderAccount", trader),
            ("perpAssetMap", context.exchange.perp_asset_map),
            ("globalTraderIndex", context.exchange.global_trader_index),
            ("activeTraderBuffer", context.exchange.active_trader_buffer),
            ("orderbook", market.market.orderbook),
            ("splines", market.market.splines),
        ],
        &serde_json::json!({
            "orderPacket": {
                "kind": {
                    "ImmediateOrCancel": {
                        "side": { side.idl_name(): null },
                        "priceInTicks": null,
                        "numBaseLots": { "inner": base_lots },
                        "numQuoteLots": null,
                        "minBaseLotsToFill": { "inner": 0 },
                        "minQuoteLotsToFill": { "inner": 0 },
                        "selfTradeBehavior": { "DecrementTake": null },
                        "matchLimit": null,
                        "clientOrderId": client_order_id,
                        "lastValidSlot": null,
                        "orderFlags": { "flags": 0 },
                        "cancelExisting": false,
                    }
                }
            }
        }),
    )?;
    run_instructions(svm, &[order])
}

/// The writes of the trader cancelling all of its resting orders in `symbol`, as Phoenix requires
/// before it liquidates anyone.
pub async fn cancel_orders(
    svm: &mut SurfnetSvm,
    remote_ctx: &Option<(SurfnetRemoteClient, CommitmentConfig)>,
    trader: Pubkey,
    symbol: &str,
) -> SurfpoolResult<Vec<(Pubkey, Account)>> {
    let context = TraderContext::load(svm, remote_ctx, trader).await?;
    let market = MarketContext::load(svm, remote_ctx, symbol).await?;
    run_instructions(
        svm,
        &[cancel_all(
            &market.exchange,
            context.authority,
            trader,
            (market.market.orderbook, market.market.splines),
        )?],
    )
}

/// The writes of the trader withdrawing `quote_lots` to its wallet's token account, paid out of
/// the global vault so what remains stays backed. Phoenix refuses more than the margin leaves.
pub async fn withdraw(
    svm: &mut SurfnetSvm,
    remote_ctx: &Option<(SurfnetRemoteClient, CommitmentConfig)>,
    trader: Pubkey,
    quote_lots: u64,
) -> SurfpoolResult<Vec<(Pubkey, Account)>> {
    let context = TraderContext::load(svm, remote_ctx, trader).await?;
    let tokens = QuoteTokens::load(svm, remote_ctx, &context).await?;
    let withdrawal = phoenix_instruction(
        "WithdrawFunds",
        &[
            ("phoenixProgram", PHOENIX_PROGRAM_ID),
            ("phoenixLogAuthority", log_authority()),
            ("globalConfiguration", PHOENIX_GLOBAL_CONFIG),
            ("traderWallet", context.authority),
            ("traderAccount", trader),
            ("perpAssetMap", context.exchange.perp_asset_map),
            ("globalVault", tokens.vault),
            ("destinationTokenAccount", tokens.wallet_account),
            ("tokenProgram", tokens.program),
            ("globalTraderIndex", context.exchange.global_trader_index),
            ("activeTraderBuffer", context.exchange.active_trader_buffer),
            ("withdrawQueue", tokens.withdraw_queue),
        ],
        &serde_json::json!({ "params": { "amount": quote_lots } }),
    )?;
    run_instructions(svm, &[tokens.open_wallet_account(&context), withdrawal])
}

/// The writes of a deposit of `quote_lots`: the quote mint's authority mints them to the trader's
/// wallet, and the trader deposits them, so the vault holds real tokens for the new collateral.
pub async fn deposit(
    svm: &mut SurfnetSvm,
    remote_ctx: &Option<(SurfnetRemoteClient, CommitmentConfig)>,
    trader: Pubkey,
    quote_lots: u64,
) -> SurfpoolResult<Vec<(Pubkey, Account)>> {
    let context = TraderContext::load(svm, remote_ctx, trader).await?;
    let tokens = QuoteTokens::load(svm, remote_ctx, &context).await?;
    let mint_authority = tokens.mint_authority.ok_or_else(|| {
        SurfpoolError::internal(format!("the quote mint {} can no longer mint", tokens.mint))
    })?;
    // Token-2022's builder takes either token program and encodes the same instruction.
    let mint = spl_token_2022_interface::instruction::mint_to(
        &tokens.program,
        &tokens.mint,
        &tokens.wallet_account,
        &mint_authority,
        &[],
        quote_lots,
    )
    .map_err(|e| SurfpoolError::internal(format!("minting the quote token: {e}")))?;
    let deposit = phoenix_instruction(
        "DepositFunds",
        &[
            ("phoenixProgram", PHOENIX_PROGRAM_ID),
            ("phoenixLogAuthority", log_authority()),
            ("globalConfiguration", PHOENIX_GLOBAL_CONFIG),
            ("traderWallet", context.authority),
            ("traderTokenAccount", tokens.wallet_account),
            ("traderAccount", trader),
            ("globalVault", tokens.vault),
            ("tokenProgram", tokens.program),
            ("globalTraderIndex", context.exchange.global_trader_index),
            ("activeTraderBuffer", context.exchange.active_trader_buffer),
        ],
        &serde_json::json!({ "params": { "amount": quote_lots } }),
    )?;
    run_instructions(svm, &[tokens.open_wallet_account(&context), mint, deposit])
}

/// The quote token accounts a deposit or withdrawal moves funds between.
struct QuoteTokens {
    mint: Pubkey,
    mint_authority: Option<Pubkey>,
    program: Pubkey,
    vault: Pubkey,
    withdraw_queue: Pubkey,
    wallet_account: Pubkey,
}

impl QuoteTokens {
    async fn load(
        svm: &mut SurfnetSvm,
        remote_ctx: &Option<(SurfnetRemoteClient, CommitmentConfig)>,
        context: &TraderContext,
    ) -> SurfpoolResult<Self> {
        let config = &context.exchange.config;
        let mint = Pubkey::new_from_array(config.canonical_token_mint_key());
        let vault = Pubkey::new_from_array(config.global_vault_key());
        let withdraw_queue = Pubkey::new_from_array(config.withdraw_queue_key());
        hydrate(svm, remote_ctx, &[mint, vault, withdraw_queue]).await?;
        let mint_account = local_account(svm, &mint)?;
        let program = mint_account.owner;
        // A Token-2022 mint keeps the classic layout ahead of its extensions.
        let base = mint_account.data.get(..Mint::LEN).ok_or_else(|| {
            SurfpoolError::invalid_account_data(mint, "Expected the quote mint", None::<String>)
        })?;
        let mint_state = Mint::unpack_from_slice(base).map_err(|e| {
            SurfpoolError::invalid_account_data(mint, "Expected the quote mint", Some(e))
        })?;
        let wallet_account =
            get_associated_token_address_with_program_id(&context.authority, &mint, &program);
        hydrate(svm, remote_ctx, &[wallet_account]).await?;
        Ok(Self {
            mint,
            mint_authority: mint_state.mint_authority.into(),
            program,
            vault,
            withdraw_queue,
            wallet_account,
        })
    }

    /// Opens the wallet's token account if it has none, paid by the wallet.
    fn open_wallet_account(&self, context: &TraderContext) -> Instruction {
        create_associated_token_account_idempotent(
            &context.authority,
            &context.authority,
            &self.mint,
            &self.program,
        )
    }
}

pub const HAWKEYE_PROGRAM_ID: Pubkey =
    Pubkey::from_str_const("RiSeVw3ZjNfsaXPRb4mgaqYaEEt41pNNJoDvVh7pgQj");
pub(crate) const VIEW_MARGIN: [u8; 8] = [0xb2, 0x0a, 0x7c, 0xad, 0xec, 0xd2, 0x75, 0x06];
const MARGIN_VIEW_MAGIC: u64 = 0x955f5b9d3dff253f;
const VIEW_MARGIN_FOR_ASSET: [u8; 8] = [0x20, 0x12, 0xaf, 0xfe, 0xa1, 0xed, 0x4d, 0x9b];
const ASSET_VIEW_MAGIC: u64 = 0xdb58fff8285e1db9;

/// What Hawkeye's `view_margin` returns for one trader.
#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
pub struct MarginView {
    magic: u64,
    pub version: u16,
    pub position_count: u16,
    pub risk_state: u8,
    pub risk_tier: u8,
    pub is_liquidatable: u8,
    padding: u8,
    pub collateral_quote_lots: i64,
    pub effective_collateral_quote_lots: i64,
    pub free_collateral_quote_lots: i64,
    pub withdrawable_collateral_quote_lots: u64,
    pub initial_margin_quote_lots: u64,
    pub maintenance_margin_quote_lots: u64,
    pub cancel_margin_quote_lots: u64,
    pub backstop_margin_quote_lots: u64,
    pub high_risk_margin_quote_lots: u64,
    pub unrealized_pnl_quote_lots: i64,
    pub discounted_unrealized_pnl_quote_lots: i64,
    pub unsettled_funding_quote_lots: i64,
}

impl MarginView {
    /// The view in `view_margin`'s return data.
    pub(crate) fn from_return_data(data: &[u8]) -> SurfpoolResult<Self> {
        read_view(data, "margin", MARGIN_VIEW_MAGIC)
    }
}

/// A Hawkeye view in its return data, which starts with the view's `magic`. Hawkeye is copied
/// from mainnet, where an upgrade may append fields, so bytes past the view are ignored.
fn read_view<T: Pod>(data: &[u8], view: &str, magic: u64) -> SurfpoolResult<T> {
    let len = size_of::<T>();
    let read = data
        .get(..len)
        .map(bytemuck::pod_read_unaligned)
        .ok_or_else(|| {
            SurfpoolError::internal(format!(
                "Hawkeye's {view} view returned {} bytes, fewer than its {len}",
                data.len()
            ))
        })?;
    let found: u64 = bytemuck::pod_read_unaligned(&data[..8]);
    if found != magic {
        return Err(SurfpoolError::internal(format!(
            "Hawkeye's {view} view returned magic {found:#x}"
        )));
    }
    Ok(read)
}

/// What Hawkeye's `view_margin_for_asset` returns for one trader in one market.
#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
pub struct AssetView {
    magic: u64,
    pub asset_id: u32,
    pub version: u16,
    pub has_position_or_orders: u8,
    padding: u8,
    pub risk_state: u8,
    pub risk_tier: u8,
    padding1: [u8; 6],
    /// Positive for a long, negative for a short.
    pub base_lots: i64,
    pub virtual_quote_lots: i64,
    pub mark_price_ticks: u64,
    pub entry_price_quote_lots_per_base_lot: u64,
    pub position_value_quote_lots: i64,
    pub unrealized_pnl_quote_lots: i64,
    pub discounted_unrealized_pnl_quote_lots: i64,
    pub unsettled_funding_quote_lots: i64,
    pub initial_margin_quote_lots: u64,
    pub maintenance_margin_quote_lots: u64,
    pub cancel_margin_quote_lots: u64,
    pub backstop_margin_quote_lots: u64,
    pub high_risk_margin_quote_lots: u64,
}

impl AssetView {
    /// The view in `view_margin_for_asset`'s return data.
    pub fn from_return_data(data: &[u8]) -> SurfpoolResult<Self> {
        read_view(data, "asset", ASSET_VIEW_MAGIC)
    }
}

pub(crate) const VIEW_BBO: [u8; 8] = [0x37, 0x5f, 0x23, 0x2d, 0x53, 0xaf, 0x12, 0x52];
const BBO_VIEW_MAGIC: u64 = 0xefca1fa31fa74171;

/// What Hawkeye's BBO view returns for one market, its makers' splines included.
#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
pub struct BboView {
    magic: u64,
    pub version: u16,
    pub flags: u8,
    padding: [u8; 5],
    pub best_bid_ticks: u64,
    pub best_ask_ticks: u64,
    pub mark_price_ticks: u64,
    pub index_price_ticks: u64,
    pub mark_price_last_updated_slot: u64,
    pub index_price_last_updated_slot: u64,
}

impl BboView {
    /// The view in the BBO view's return data.
    pub fn from_return_data(data: &[u8]) -> SurfpoolResult<Self> {
        read_view(data, "BBO", BBO_VIEW_MAGIC)
    }

    /// Whether a bid still rests above an ask. A side reported as 0 has no price to cross.
    pub fn crossed(&self) -> bool {
        self.best_bid_ticks != 0
            && self.best_ask_ticks != 0
            && self.best_bid_ticks > self.best_ask_ticks
    }
}

/// One market a trader has a position or resting orders in, as Hawkeye reports it.
pub struct Holding {
    pub market: Market,
    /// Positive for a long, negative for a short, 0 for resting orders alone.
    pub base_lots: i64,
    pub maintenance_margin_quote_lots: u64,
}

/// Every listed market where Hawkeye reports a position or resting orders of `trader`. Hawkeye
/// reads positions where Phoenix keeps them, and the Trader account's own copy can miss markets.
pub fn holdings(
    svm: &SurfnetSvm,
    exchange: &Exchange,
    trader: &Pubkey,
) -> SurfpoolResult<Vec<Holding>> {
    let map = local_account(svm, &exchange.perp_asset_map)?;
    let markets = phoenix_markets(exchange.perp_asset_map, &map)?;
    holdings_in(&mut Sandbox::new(svm), exchange, trader, &markets)
}

/// [`holdings`] among `markets`, read in `views`, which Hawkeye's read-only views can share.
fn holdings_in(
    views: &mut Sandbox,
    exchange: &Exchange,
    trader: &Pubkey,
    markets: &[Market],
) -> SurfpoolResult<Vec<Holding>> {
    let mut held = Vec::new();
    for market in markets {
        let data = views.view(&asset_view(exchange, trader, market.asset_id)?)?;
        let view = AssetView::from_return_data(&data)?;
        if view.has_position_or_orders != 0 {
            held.push(Holding {
                market: market.clone(),
                base_lots: view.base_lots,
                maintenance_margin_quote_lots: view.maintenance_margin_quote_lots,
            });
        }
    }
    Ok(held)
}

/// The most position holders a cascade examines.
const MAX_CASCADE_CANDIDATES: usize = 24;

/// Stands in for the user's keeper, which signs the real liquidation.
const PLACEHOLDER_LIQUIDATOR: Pubkey = Pubkey::new_from_array([1; 32]);

/// Traders a cascade leaves liquidatable at one price, and where the market was moved for them.
pub struct CascadeReady {
    pub writes: Vec<(Pubkey, Account)>,
    pub target_ticks: u64,
    /// Each trader with the liquidation that went through for it, in this order, on a copy of
    /// the prepared state.
    pub liquidations: Vec<(Pubkey, Instruction)>,
}

/// The writes that move `symbol` inside the most liquidation bands of holders in Phoenix's active
/// trader index, so a keeper can liquidate them in turn; one failing on a copy is left out.
pub async fn prepare_cascade(
    svm: &mut SurfnetSvm,
    remote_ctx: &Option<(SurfnetRemoteClient, CommitmentConfig)>,
    symbol: &str,
    long: bool,
) -> SurfpoolResult<CascadeReady> {
    let mut context = MarketContext::load(svm, remote_ctx, symbol).await?;
    hydrate(svm, remote_ctx, &[HAWKEYE_PROGRAM_ID]).await?;
    let index = local_account(svm, &context.exchange.global_trader_index)?;
    let mut listed: Vec<Pubkey> = index_trader_state_ranges(&index)?
        .into_iter()
        .map(|(trader, _)| trader)
        .collect();
    listed.sort_unstable();
    listed.dedup();
    hydrate(svm, remote_ctx, &listed).await?;
    let map = local_account(svm, &context.exchange.perp_asset_map)?;
    let markets = phoenix_markets(context.exchange.perp_asset_map, &map)?;
    let mut candidates = Vec::new();
    let mut views = Sandbox::new(svm);
    for trader in listed {
        // Hawkeye, not the Trader account's copy, which can miss a hot trader's positions.
        let Ok(position) = views
            .view(&asset_view(
                &context.exchange,
                &trader,
                context.market.asset_id,
            )?)
            .and_then(|data| AssetView::from_return_data(&data))
        else {
            continue;
        };
        let lots = position.base_lots;
        if (long && lots > 0) || (!long && lots < 0) {
            let account = local_account(svm, &trader)?;
            let Ok(header) = TraderHeader::try_read_from_account_bytes(&account.data) else {
                continue;
            };
            let authority = Pubkey::new_from_array(header.authority);
            let Ok(held) = holdings_in(&mut views, &context.exchange, &trader, &markets) else {
                continue;
            };
            candidates.push((trader, authority, held));
        }
        if candidates.len() == MAX_CASCADE_CANDIDATES {
            break;
        }
    }
    drop(views);
    let books: Vec<Pubkey> = candidates
        .iter()
        .flat_map(|(_, _, held)| held_books(held))
        .collect();
    hydrate(svm, remote_ctx, &books).await?;
    let holders = candidates
        .iter()
        .map(|(trader, authority, held)| {
            Ok((
                *trader,
                cancels(&context.exchange, *authority, *trader, held)?,
            ))
        })
        .collect::<SurfpoolResult<Vec<(Pubkey, Vec<Instruction>)>>>()?;

    let mark = context.market.mark_ticks;
    let far = if long {
        1
    } else {
        mark.saturating_mul(10).min(u64::from(u32::MAX))
    };
    let precision = (mark / 2000).max(1);
    let base = Sandbox::new(svm);
    let mut bands = Vec::new();
    for (trader, cancels) in &holders {
        let mut search = base.trial();
        if search.run(cancels).is_err() {
            continue;
        }
        search.next_slot();
        search.run(&context.oracle_reports(mark)?)?;
        let Ok(now) = margin(&mut search, &context.exchange, trader) else {
            continue;
        };
        // A holder already liquidatable at the mark has a band that starts there.
        let liquidatable = if now.is_liquidatable == 1 {
            if now.effective_collateral_quote_lots <= 0 {
                continue;
            }
            mark
        } else {
            let Some(price) =
                boundary(&mut search, &mut context, trader, far, precision, |view| {
                    view.is_liquidatable == 1
                })?
            else {
                continue;
            };
            price
        };
        let underwater = boundary(&mut search, &mut context, trader, far, precision, |view| {
            view.effective_collateral_quote_lots <= 0
        })?
        .unwrap_or(far);
        // Keep a quarter of the band away from underwater: the fill lands a little through the mark.
        let (low, high) = if long {
            (
                underwater + liquidatable.abs_diff(underwater) / 4 + 1,
                liquidatable.saturating_sub(precision),
            )
        } else {
            (
                liquidatable + precision,
                underwater.saturating_sub(underwater.abs_diff(liquidatable) / 4 + 1),
            )
        };
        if low <= high {
            bands.push((*trader, low, high));
        }
    }

    // The price inside the most bands, the smallest move among equals.
    let target = bands
        .iter()
        .flat_map(|(_, low, high)| [*low, *high])
        .max_by_key(|price| {
            let inside = bands
                .iter()
                .filter(|(_, low, high)| (low..=high).contains(&price))
                .count();
            (inside, std::cmp::Reverse(price.abs_diff(mark)))
        })
        .ok_or_else(|| {
            SurfpoolError::internal(format!(
                "no {} {symbol} holder becomes liquidatable before it goes underwater",
                if long { "long" } else { "short" }
            ))
        })?;
    let mut chosen: Vec<Pubkey> = bands
        .iter()
        .filter(|(_, low, high)| (low..=high).contains(&&target))
        .map(|(trader, ..)| *trader)
        .collect();

    // A trader the book cannot absorb in turn is dropped, and the rest are prepared again.
    while !chosen.is_empty() {
        let mut prepared = base.trial();
        for (trader, cancels) in &holders {
            if chosen.contains(trader) {
                prepared.run(cancels)?;
            }
        }
        prepared.next_slot();
        context
            .move_in(&mut prepared, target)
            .map_err(explain_move_failure)?;
        let mut trial = prepared.trial();
        let mut liquidations = Vec::new();
        for trader in &chosen {
            let instruction = liquidation(&context, *trader, long, target)?;
            if trial.run(std::slice::from_ref(&instruction)).is_ok() {
                liquidations.push((*trader, instruction));
            }
        }
        if liquidations.len() == chosen.len() {
            return Ok(CascadeReady {
                writes: prepared.writes()?,
                target_ticks: target,
                liquidations,
            });
        }
        chosen = liquidations.into_iter().map(|(trader, _)| trader).collect();
    }
    Err(SurfpoolError::internal(format!(
        "no liquidation of a {symbol} holder goes through at {target} ticks"
    )))
}

/// A cross-margin move is in millionths of each market's mark.
const MOVE_SCALE: u64 = 1_000_000;
/// How finely the cross-margin search places its boundaries, in millionths of each mark.
const MOVE_PRECISION: u64 = 100;

/// A trader whose positions a keeper can liquidate one after another, and where their markets
/// were moved for it.
pub struct CrossMarginReady {
    pub writes: Vec<(Pubkey, Account)>,
    /// Each position's market and the ticks it was moved to, in the order they liquidate.
    pub moves: Vec<(String, u64)>,
    /// The liquidations that went through one after another on a copy of the prepared state, in
    /// this order.
    pub liquidations: Vec<Instruction>,
}

/// The writes that leave `trader` ready for a keeper to liquidate its `symbols` positions in turn,
/// largest maintenance margin last, each market moved one share of its mark against them.
pub async fn prepare_cross_margin(
    svm: &mut SurfnetSvm,
    remote_ctx: &Option<(SurfnetRemoteClient, CommitmentConfig)>,
    trader: Pubkey,
    symbols: &[&str],
) -> SurfpoolResult<CrossMarginReady> {
    if symbols.is_empty() {
        return Err(SurfpoolError::internal("list at least one market symbol"));
    }
    refuse_repeats(symbols)?;
    hydrate(svm, remote_ctx, &[trader]).await?;
    let header = TraderHeader::try_read_from_account_bytes(&local_account(svm, &trader)?.data)
        .map_err(|e| {
            SurfpoolError::invalid_account_data(trader, "Expected a Phoenix Trader", Some(e))
        })?;
    // Phoenix answered "position not liquidatable" for such traders across the whole band.
    if header.num_markets_with_splines > 0 {
        return Err(SurfpoolError::internal(format!(
            "{trader} quotes splines on {} markets; liquidating a spline market maker is not \
             supported",
            header.num_markets_with_splines
        )));
    }
    let exchange = Exchange::load(svm, remote_ctx).await?;
    hydrate(
        svm,
        remote_ctx,
        &[
            HAWKEYE_PROGRAM_ID,
            exchange.perp_asset_map,
            exchange.global_trader_index,
            exchange.active_trader_buffer,
        ],
    )
    .await?;
    let map = local_account(svm, &exchange.perp_asset_map)?;
    for symbol in symbols {
        Market::find(&exchange.perp_asset_map, &map.data, symbol)?;
    }
    refuse_unindexed_hot_trader(
        trader,
        &header,
        &local_account(svm, &exchange.global_trader_index)?,
    )?;
    let held = holdings(svm, &exchange, &trader)?;
    let positions = symbols
        .iter()
        .map(|symbol| {
            held.iter()
                .find(|held| held.market.symbol == *symbol && held.base_lots != 0)
                .ok_or_else(|| {
                    SurfpoolError::internal(format!("{trader} holds no {symbol} position"))
                })
        })
        .collect::<SurfpoolResult<Vec<_>>>()?;
    let contexts = MarketContext::load_all(svm, remote_ctx, symbols).await?;
    let mut chosen = Vec::with_capacity(symbols.len());
    for (holding, context) in positions.into_iter().zip(contexts) {
        chosen.push((
            holding.maintenance_margin_quote_lots,
            Chosen {
                long: holding.base_lots > 0,
                mark: context.market.mark_ticks,
                context,
            },
        ));
    }
    chosen.sort_by_key(|(maintenance, _)| *maintenance);
    let authority = Pubkey::new_from_array(header.authority);
    let cancels = cancel_everywhere(svm, remote_ctx, &exchange, trader, authority, &held).await?;

    // The cancels come first so the uncross cannot fill the trader's own orders, and the moves a
    // slot later; see `Sandbox::next_slot`.
    let mut base = Sandbox::new(svm);
    base.run(&cancels)?;
    base.next_slot();
    let mut search = CrossMarginSearch {
        base,
        exchange,
        trader,
        chosen: chosen.into_iter().map(|(_, chosen)| chosen).collect(),
    };
    let band = search.band()?;
    search.ready_in(band)?.ok_or_else(|| {
        SurfpoolError::internal(format!(
            "no common move leaves {trader} liquidatable in turn on {}",
            symbols.join(", ")
        ))
    })
}

/// One position a cross-margin run liquidates, with the market it moves.
struct Chosen {
    context: MarketContext,
    long: bool,
    mark: u64,
}

impl Chosen {
    /// The price `moved` millionths of the mark against the position.
    fn against(&self, moved: u64) -> u64 {
        let mark = u128::from(self.mark);
        let factor = if self.long {
            MOVE_SCALE.saturating_sub(moved)
        } else {
            MOVE_SCALE + moved
        };
        let price = mark * u128::from(factor) / u128::from(MOVE_SCALE);
        u64::try_from(price)
            .unwrap_or(u64::MAX)
            .clamp(1, u64::from(u32::MAX))
    }
}

struct CrossMarginSearch<'a> {
    base: Sandbox<'a>,
    exchange: Exchange,
    trader: Pubkey,
    chosen: Vec<Chosen>,
}

impl<'a> CrossMarginSearch<'a> {
    /// The moves from liquidatable to underwater, where market-order liquidations stop. The
    /// margin reads only the marks, so this moves the oracles alone.
    fn band(&mut self) -> SurfpoolResult<(u64, u64)> {
        let farthest = if self.chosen.iter().any(|chosen| chosen.long) {
            MOVE_SCALE - MOVE_SCALE / 100
        } else {
            9 * MOVE_SCALE
        };
        let mut search = self.base.trial();
        let mut margin_at = |moved: u64| -> SurfpoolResult<MarginView> {
            for chosen in &mut self.chosen {
                let price = chosen.against(moved);
                search.run(&chosen.context.oracle_reports(price)?)?;
            }
            margin(&mut search, &self.exchange, &self.trader)
        };
        let liquidatable =
            first_move(farthest, |moved| Ok(margin_at(moved)?.is_liquidatable == 1))?.ok_or_else(
                || {
                    SurfpoolError::internal(format!(
                        "{} never becomes liquidatable on these markets",
                        self.trader
                    ))
                },
            )?;
        let underwater = first_move(farthest, |moved| {
            Ok(margin_at(moved)?.effective_collateral_quote_lots <= 0)
        })?
        .unwrap_or(farthest);
        if underwater <= liquidatable {
            return Err(SurfpoolError::internal(format!(
                "{} goes underwater before it becomes liquidatable",
                self.trader
            )));
        }
        Ok((liquidatable, underwater))
    }

    /// Every chosen market moved `moved` against its position, and their liquidations tried in
    /// turn on a copy. Each closes at most the max liquidation size of its market.
    fn run_at(&mut self, moved: u64) -> SurfpoolResult<Run<'a>> {
        let mut prepared = self.base.trial();
        for chosen in &mut self.chosen {
            let price = chosen.against(moved);
            chosen
                .context
                .move_in(&mut prepared, price)
                .map_err(explain_move_failure)?;
        }
        let mut trial = prepared.trial();
        let mut liquidations = Vec::with_capacity(self.chosen.len());
        for chosen in &self.chosen {
            let instruction = liquidation(
                &chosen.context,
                self.trader,
                chosen.long,
                chosen.against(moved),
            )?;
            if trial.run(std::slice::from_ref(&instruction)).is_err() {
                return Ok(
                    if margin(&mut trial, &self.exchange, &self.trader)?.is_liquidatable == 0 {
                        Run::Recovered
                    } else {
                        Run::Refused
                    },
                );
            }
            liquidations.push(instruction);
        }
        Ok(Run::Through(Box::new(prepared), liquidations))
    }

    fn goes_through(&mut self, moved: u64) -> SurfpoolResult<bool> {
        Ok(matches!(self.run_at(moved)?, Run::Through(..)))
    }

    /// The state mid-way through the moves in `band` where the whole run goes through, if any. A
    /// recovered account needs a deeper move and a refusal a shallower one, so the band halves.
    fn ready_in(
        &mut self,
        (liquidatable, underwater): (u64, u64),
    ) -> SurfpoolResult<Option<CrossMarginReady>> {
        let (mut shallow, mut deep) = (liquidatable, underwater);
        let (found, found_run) = loop {
            if deep - shallow <= MOVE_PRECISION {
                return Ok(None);
            }
            let moved = midpoint(shallow, deep);
            match self.run_at(moved)? {
                Run::Through(prepared, liquidations) => break (moved, (prepared, liquidations)),
                Run::Recovered => shallow = moved,
                Run::Refused => deep = moved,
            }
        };
        let low = bisect(shallow, found, MOVE_PRECISION, |moved| {
            self.goes_through(moved)
        })?
        .1;
        let high = bisect(deep, found, MOVE_PRECISION, |moved| {
            self.goes_through(moved)
        })?
        .1;

        let middle = midpoint(low, high);
        let middle_run = (middle != found).then(|| self.run_at(middle)).transpose()?;
        let (moved, (prepared, liquidations)) = match middle_run {
            Some(Run::Through(prepared, liquidations)) => (middle, (prepared, liquidations)),
            _ => (found, found_run),
        };
        let moves = self
            .chosen
            .iter()
            .map(|chosen| (chosen.context.market.symbol.clone(), chosen.against(moved)))
            .collect();
        Ok(Some(CrossMarginReady {
            writes: prepared.writes()?,
            moves,
            liquidations,
        }))
    }
}

/// How a cross-margin run at one move ended.
enum Run<'a> {
    /// Every liquidation went through, on this prepared state.
    Through(Box<Sandbox<'a>>, Vec<Instruction>),
    /// A liquidation failed because the account was no longer liquidatable.
    Recovered,
    /// A liquidation failed while the account was liquidatable.
    Refused,
}

/// The smallest move up to `farthest` where `reached` holds, within [`MOVE_PRECISION`], or
/// `None` when it does not hold even there.
fn first_move(
    farthest: u64,
    mut reached: impl FnMut(u64) -> SurfpoolResult<bool>,
) -> SurfpoolResult<Option<u64>> {
    if reached(0)? {
        return Ok(Some(0));
    }
    if !reached(farthest)? {
        return Ok(None);
    }
    Ok(Some(bisect(0, farthest, MOVE_PRECISION, reached)?.1))
}

/// Narrows `missed`, where `reached` does not hold, and `hit`, where it does, to within
/// `precision` of each other.
fn bisect(
    mut missed: u64,
    mut hit: u64,
    precision: u64,
    mut reached: impl FnMut(u64) -> SurfpoolResult<bool>,
) -> SurfpoolResult<(u64, u64)> {
    while missed.abs_diff(hit) > precision {
        let candidate = midpoint(missed, hit);
        if reached(candidate)? {
            hit = candidate;
        } else {
            missed = candidate;
        }
    }
    Ok((missed, hit))
}

/// The price closest to the mark, on the way to `far`, where `reached` first holds for `trader`,
/// or `None` when it does not hold even at `far`.
fn boundary(
    search: &mut Sandbox,
    context: &mut MarketContext,
    trader: &Pubkey,
    far: u64,
    precision: u64,
    reached: impl Fn(&MarginView) -> bool,
) -> SurfpoolResult<Option<u64>> {
    let mark = context.market.mark_ticks;
    let mut reached_at = |price: u64| -> SurfpoolResult<bool> {
        search.run(&context.oracle_reports(price)?)?;
        Ok(reached(&margin(search, &context.exchange, trader)?))
    };
    if !reached_at(far)? {
        return Ok(None);
    }
    Ok(Some(bisect(mark, far, precision, reached_at)?.1))
}

/// The price halfway between two, strictly between them whenever they are two or more apart.
fn midpoint(a: u64, b: u64) -> u64 {
    a.min(b) + a.abs_diff(b) / 2
}

fn cancel_all(
    exchange: &Exchange,
    authority: Pubkey,
    trader: Pubkey,
    (orderbook, splines): (Pubkey, Pubkey),
) -> SurfpoolResult<Instruction> {
    phoenix_instruction(
        "CancelAll",
        &[
            ("phoenixProgram", PHOENIX_PROGRAM_ID),
            ("phoenixLogAuthority", log_authority()),
            ("globalConfiguration", PHOENIX_GLOBAL_CONFIG),
            ("traderWallet", authority),
            ("traderAccount", trader),
            ("perpAssetMap", exchange.perp_asset_map),
            ("globalTraderIndex", exchange.global_trader_index),
            ("activeTraderBuffer", exchange.active_trader_buffer),
            ("orderbook", orderbook),
            ("splines", splines),
        ],
        &serde_json::json!({}),
    )
}

/// The trader cancelling its resting orders on every market in `held`: Phoenix refuses to
/// liquidate a trader with risk-increasing orders on any market.
async fn cancel_everywhere(
    svm: &mut SurfnetSvm,
    remote_ctx: &Option<(SurfnetRemoteClient, CommitmentConfig)>,
    exchange: &Exchange,
    trader: Pubkey,
    authority: Pubkey,
    held: &[Holding],
) -> SurfpoolResult<Vec<Instruction>> {
    hydrate(svm, remote_ctx, &held_books(held)).await?;
    cancels(exchange, authority, trader, held)
}

fn held_books(held: &[Holding]) -> Vec<Pubkey> {
    held.iter()
        .flat_map(|held| [held.market.orderbook, held.market.splines])
        .collect()
}

fn cancels(
    exchange: &Exchange,
    authority: Pubkey,
    trader: Pubkey,
    held: &[Holding],
) -> SurfpoolResult<Vec<Instruction>> {
    held.iter()
        .map(|held| {
            cancel_all(
                exchange,
                authority,
                trader,
                (held.market.orderbook, held.market.splines),
            )
        })
        .collect()
}

/// A keeper's liquidation of as much of the position as the market allows in one go, priced like
/// the keepers on mainnet: at most 10% through the mark.
pub fn liquidation(
    context: &MarketContext,
    trader: Pubkey,
    long: bool,
    mark_ticks: u64,
) -> SurfpoolResult<Instruction> {
    let limit = if long {
        mark_ticks - mark_ticks / 10
    } else {
        mark_ticks + mark_ticks / 10
    }
    .clamp(1, u64::from(u32::MAX));
    phoenix_instruction(
        "LiquidateViaMarketOrder",
        &[
            ("phoenixProgram", PHOENIX_PROGRAM_ID),
            ("phoenixLogAuthority", log_authority()),
            ("globalConfiguration", PHOENIX_GLOBAL_CONFIG),
            ("liquidatorWallet", PLACEHOLDER_LIQUIDATOR),
            ("liquidatedTrader", trader),
            ("traderAccount", trader),
            ("perpAssetMap", context.exchange.perp_asset_map),
            ("globalTraderIndex", context.exchange.global_trader_index),
            ("activeTraderBuffer", context.exchange.active_trader_buffer),
            ("orderbook", context.market.orderbook),
            ("splines", context.market.splines),
        ],
        &serde_json::json!({
            "params": {
                "assetMint": Pubkey::new_from_array(context.exchange.config.canonical_token_mint_key()).to_string(),
                "liquidationSize": { "inner": context.market.max_liquidation_size },
                "liquidationPrice": { "inner": limit },
                "fillOrKill": false,
            }
        }),
    )
}

/// Hawkeye's margin view of `trader`, read inside the sandbox.
pub fn margin(
    sandbox: &mut Sandbox,
    exchange: &Exchange,
    trader: &Pubkey,
) -> SurfpoolResult<MarginView> {
    let data = sandbox.view(&hawkeye_view(exchange, &[*trader], VIEW_MARGIN.to_vec()))?;
    MarginView::from_return_data(&data)
}

/// Hawkeye's BBO view of `market`, read inside the sandbox.
pub fn bbo(sandbox: &mut Sandbox, exchange: &Exchange, market: &Market) -> SurfpoolResult<BboView> {
    let books = [market.orderbook, market.splines];
    BboView::from_return_data(&sandbox.view(&hawkeye_view(exchange, &books, VIEW_BBO.to_vec()))?)
}

/// Hawkeye's `view_margin_for_asset` of `trader` in the market with `asset_id`.
pub fn asset_view(
    exchange: &Exchange,
    trader: &Pubkey,
    asset_id: u64,
) -> SurfpoolResult<Instruction> {
    let asset_id = u32::try_from(asset_id).map_err(|_| {
        SurfpoolError::internal(format!("asset id {asset_id} does not fit Hawkeye's u32"))
    })?;
    let mut data = VIEW_MARGIN_FOR_ASSET.to_vec();
    data.extend(asset_id.to_le_bytes());
    data.extend([0; 4]);
    Ok(hawkeye_view(exchange, &[*trader], data))
}

/// A Hawkeye view over the exchange accounts and `extra`; `data` is the view's discriminator and
/// its parameters.
fn hawkeye_view(exchange: &Exchange, extra: &[Pubkey], data: Vec<u8>) -> Instruction {
    let accounts = [
        PHOENIX_PROGRAM_ID,
        PHOENIX_GLOBAL_CONFIG,
        exchange.global_trader_index,
        exchange.active_trader_buffer,
        exchange.perp_asset_map,
    ]
    .iter()
    .chain(extra)
    .map(|address| AccountMeta::new_readonly(*address, false))
    .collect();
    Instruction {
        program_id: HAWKEYE_PROGRAM_ID,
        accounts,
        data,
    }
}

/// One position of a trader, as a surfnet reports it.
pub struct TraderPosition {
    pub symbol: String,
    pub orderbook: Pubkey,
    /// Positive for a long, negative for a short.
    pub base_lots: i64,
    pub maintenance_margin_quote_lots: u64,
}

/// Every listed market `trader` holds a position in, as Hawkeye reports it on a copy of the
/// surfnet `client` points at, which loads any account it is missing; nothing is written back.
pub async fn trader_positions(
    client: SurfnetRemoteClient,
    trader: Pubkey,
) -> SurfpoolResult<Vec<TraderPosition>> {
    let (mut svm, _events, _geyser) = SurfnetSvm::new(SurfnetSvmConfig::default())?;
    let commitment = CommitmentConfig::confirmed();
    let clock = client
        .get_account(&solana_sysvar::clock::ID, commitment)
        .await?
        .map_account()?;
    let clock: Clock = bincode::deserialize(&clock.data)
        .map_err(|e| SurfpoolError::internal(format!("the surfnet's Clock is invalid: {e}")))?;
    svm.inner.set_sysvar(&clock);
    svm.inner.set_sysvar(&client.get_last_restart_slot().await?);
    let remote = Some((client, commitment));
    let exchange = Exchange::load(&mut svm, &remote).await?;
    hydrate(
        &mut svm,
        &remote,
        &[
            trader,
            HAWKEYE_PROGRAM_ID,
            exchange.perp_asset_map,
            exchange.global_trader_index,
            exchange.active_trader_buffer,
        ],
    )
    .await?;
    let account = svm.inner.get_account(&trader)?.ok_or_else(|| {
        SurfpoolError::internal(format!("there is no Phoenix Trader at {trader}"))
    })?;
    if account.owner != PHOENIX_PROGRAM_ID {
        return Err(SurfpoolError::invalid_account_owner(
            trader,
            Some("Expected a Phoenix Trader"),
        ));
    }
    TraderHeader::try_read_from_account_bytes(&account.data).map_err(|e| {
        SurfpoolError::invalid_account_data(trader, "Expected a Phoenix Trader", Some(e))
    })?;
    keep_markets_usable(&mut svm, &remote, clock.slot).await;
    Ok(holdings(&svm, &exchange, &trader)?
        .into_iter()
        .filter(|held| held.base_lots != 0)
        .map(|held| TraderPosition {
            symbol: held.market.symbol,
            orderbook: held.market.orderbook,
            base_lots: held.base_lots,
            maintenance_margin_quote_lots: held.maintenance_margin_quote_lots,
        })
        .collect())
}

fn refuse_repeats(symbols: &[&str]) -> SurfpoolResult<()> {
    match symbols
        .iter()
        .enumerate()
        .find_map(|(index, symbol)| symbols[..index].contains(symbol).then_some(symbol))
    {
        Some(repeated) => Err(SurfpoolError::internal(format!(
            "{repeated} is listed twice"
        ))),
        None => Ok(()),
    }
}

/// Phoenix looks a hot trader up in the index, and fails its cancels and liquidations with
/// TraderNotFound when it is missing there.
fn refuse_unindexed_hot_trader(
    trader: Pubkey,
    header: &TraderHeader,
    index: &Account,
) -> SurfpoolResult<()> {
    if header.trader_state.is_hot()
        && !index_trader_state_ranges(index)?
            .iter()
            .any(|(listed, _)| *listed == trader)
    {
        return Err(SurfpoolError::internal(format!(
            "{trader} is marked hot, but the local GlobalTraderIndex does not list it, so Phoenix \
             can neither cancel its orders nor liquidate it; the two accounts were fetched at \
             different times. Restart the surfnet (or reset the Phoenix accounts) and play again"
        )));
    }
    Ok(())
}

/// Every hot Trader the GlobalTraderIndex tree reaches, with its TraderState byte range. Freed
/// nodes keep stale keys and collateral, so the walk starts at the root; a repeated key fails.
pub fn index_trader_state_ranges(index: &Account) -> SurfpoolResult<Vec<(Pubkey, Range<usize>)>> {
    let invalid = || SurfpoolError::internal("Invalid Phoenix GlobalTraderIndex tree");
    if index.owner != PHOENIX_PROGRAM_ID {
        return Err(SurfpoolError::internal(
            "Expected a Phoenix-owned GlobalTraderIndex account",
        ));
    }
    let header = MultiArenaHeader::try_from_account_bytes(
        "GlobalTraderIndex",
        &index.data,
        PhoenixAccount::GlobalTraderIndexHeader.discriminant(),
    )
    .map_err(|error| {
        SurfpoolError::internal(format!("Invalid Phoenix GlobalTraderIndex: {error}"))
    })?;
    if header.num_arenas() != 1 || header.superblock().num_active_arenas() != 1 {
        return Err(SurfpoolError::internal(
            "Phoenix trader lookups require a single-arena GlobalTraderIndex",
        ));
    }
    // MultiArenaHeader (48), superblock (32), tree root and padding (16), then
    // 1-based Sokoban nodes: four u32 registers, a 32-byte key, and IDL TraderState.
    const NODES_START: usize = 96;
    const NODE_LEN: usize = 64;
    let data = &index.data;
    if data.len() < NODES_START || !(data.len() - NODES_START).is_multiple_of(NODE_LEN) {
        return Err(invalid());
    }
    let capacity = (data.len() - NODES_START) / NODE_LEN;
    let read_u32 = |offset| u32::from_le_bytes(data[offset..offset + 4].try_into().unwrap());
    let mut pending = vec![read_u32(80)];
    let mut visited = HashSet::new();
    let mut keys = HashSet::new();
    let mut ranges = Vec::new();
    while let Some(node) = pending.pop() {
        if node == 0 {
            continue;
        }
        if node >= header.superblock().bump_index()
            || node as usize > capacity
            || !visited.insert(node)
        {
            return Err(invalid());
        }
        let start = NODES_START + (node as usize - 1) * NODE_LEN;
        pending.extend([read_u32(start), read_u32(start + 4)]);
        let key = Pubkey::new_from_array(data[start + 16..start + 48].try_into().unwrap());
        if !keys.insert(key) {
            return Err(invalid());
        }
        ranges.push((key, start + 48..start + 64));
    }
    if visited.len() != header.superblock().size() as usize {
        return Err(invalid());
    }
    Ok(ranges)
}

#[cfg(test)]
pub(crate) mod tests {
    use phoenix_rise_accounts::trader::TRADER_CAPABILITY_HOT;

    use super::*;

    pub(crate) fn index_trader_state_range(
        index: &Account,
        trader_key: &[u8; 32],
    ) -> SurfpoolResult<Range<usize>> {
        let trader_key = Pubkey::new_from_array(*trader_key);
        index_trader_state_ranges(index)?
            .into_iter()
            .find_map(|(key, range)| (key == trader_key).then_some(range))
            .ok_or_else(|| {
                SurfpoolError::internal(
                    "Hot Phoenix Trader has no reachable GlobalTraderIndex entry",
                )
            })
    }

    #[test]
    fn a_midpoint_always_narrows_the_search() {
        // Two odd ends two apart once stalled the halving at one of them.
        assert_eq!(midpoint(3, 1), 2);
        assert_eq!(midpoint(1, 3), 2);
        assert_eq!(midpoint(10_400, 10_402), 10_401);
        assert_eq!(midpoint(1, u64::from(u32::MAX)), 2_147_483_648);
        assert_eq!(midpoint(7, 8), 7);
    }

    #[test]
    fn a_view_with_appended_fields_decodes_and_a_short_one_is_refused() {
        let view = MarginView {
            magic: MARGIN_VIEW_MAGIC,
            collateral_quote_lots: 42,
            ..MarginView::zeroed()
        };
        let mut data = bytemuck::bytes_of(&view).to_vec();
        data.extend([7; 16]);
        let read = MarginView::from_return_data(&data).unwrap();
        assert_eq!(read.collateral_quote_lots, 42);

        let short = &data[..size_of::<MarginView>() - 1];
        assert!(MarginView::from_return_data(short).is_err());
        assert!(AssetView::from_return_data(&data).is_err(), "wrong magic");
    }

    #[test]
    fn a_cross_margin_search_finds_the_first_move() {
        let found = first_move(990_000, |moved| Ok(moved >= 351_088)).unwrap();
        assert!(found.is_some_and(|moved| (351_088..=351_088 + MOVE_PRECISION).contains(&moved)));
        assert_eq!(first_move(990_000, |_| Ok(true)).unwrap(), Some(0));
        assert_eq!(first_move(990_000, |_| Ok(false)).unwrap(), None);
    }

    const FIRST_KEY: [u8; 32] = [11; 32];
    const SECOND_KEY: [u8; 32] = [22; 32];

    fn write_u32(data: &mut [u8], offset: usize, value: u32) {
        data[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
    }

    fn index_account() -> Account {
        let mut data = vec![0; 96 + 3 * 64];
        data[..8].copy_from_slice(&PhoenixAccount::GlobalTraderIndexHeader.discriminant());
        write_u32(&mut data, 48, 2);
        data[52..54].copy_from_slice(&1_u16.to_le_bytes());
        data[54..56].copy_from_slice(&1_u16.to_le_bytes());
        write_u32(&mut data, 56, 3);
        write_u32(&mut data, 60, 4);
        write_u32(&mut data, 64, 3);
        write_u32(&mut data, 80, 2);
        write_u32(&mut data, 160, 1);
        write_u32(&mut data, 104, 2);
        for (slot, key, collateral) in [
            (0, FIRST_KEY, 111_i64),
            (1, SECOND_KEY, 222_i64),
            (2, FIRST_KEY, 999_i64),
        ] {
            let start = 96 + slot * 64;
            data[start + 16..start + 48].copy_from_slice(&key);
            data[start + 48..start + 56].copy_from_slice(&collateral.to_le_bytes());
            write_u32(&mut data, start + 56, TRADER_CAPABILITY_HOT);
        }
        Account {
            data,
            owner: PHOENIX_PROGRAM_ID,
            ..Account::default()
        }
    }

    #[test]
    fn a_hot_trader_the_index_does_not_list_is_refused() {
        let index = index_account();
        let mut header = TraderHeader::zeroed();
        header.trader_state.flags = TRADER_CAPABILITY_HOT;
        let listed = Pubkey::new_from_array(FIRST_KEY);
        let missing = Pubkey::new_unique();
        assert!(refuse_unindexed_hot_trader(listed, &header, &index).is_ok());
        let error = refuse_unindexed_hot_trader(missing, &header, &index)
            .unwrap_err()
            .to_string();
        assert!(
            error.contains(&format!(
                "{missing} is marked hot, but the local GlobalTraderIndex does not list it"
            )),
            "{error}"
        );
        header.trader_state.flags = 0;
        assert!(
            refuse_unindexed_hot_trader(missing, &header, &index).is_ok(),
            "a cold trader is not looked up"
        );
    }

    #[test]
    fn index_lookup_selects_reachable_keys_and_rejects_malformed_trees() {
        let index = index_account();
        assert_eq!(
            index_trader_state_range(&index, &FIRST_KEY).unwrap(),
            144..160
        );
        assert_eq!(
            index_trader_state_range(&index, &SECOND_KEY).unwrap(),
            208..224
        );
        assert!(index_trader_state_range(&index, &[44; 32]).is_err());

        type Corrupt = fn(&mut Account);
        let corruptions: [(&str, Corrupt); 8] = [
            ("only a freed duplicate matches", |index| {
                index.data[112..144].copy_from_slice(&[33; 32])
            }),
            ("cycle", |index| write_u32(&mut index.data, 96, 2)),
            ("size disagrees with the reachable nodes", |index| {
                write_u32(&mut index.data, 48, 3)
            }),
            ("child beyond capacity", |index| {
                write_u32(&mut index.data, 60, 100);
                write_u32(&mut index.data, 164, 4);
            }),
            ("child above the bump index", |index| {
                write_u32(&mut index.data, 60, 2)
            }),
            ("duplicate reachable key", |index| {
                index.data[176..208].copy_from_slice(&FIRST_KEY)
            }),
            ("wrong owner", |index| index.owner = Pubkey::new_unique()),
            ("wrong discriminator", |index| index.data[..8].fill(0)),
        ];
        for (case, corrupt) in corruptions {
            let mut index = index_account();
            corrupt(&mut index);
            assert!(
                index_trader_state_range(&index, &FIRST_KEY).is_err(),
                "{case}"
            );
        }
        for (arenas, active) in [(0_u16, 1_u16), (2, 1), (1, 0), (1, 2)] {
            let mut index = index_account();
            index.data[52..54].copy_from_slice(&arenas.to_le_bytes());
            index.data[54..56].copy_from_slice(&active.to_le_bytes());
            assert!(index_trader_state_range(&index, &FIRST_KEY).is_err());
        }
        for len in [0, 79, 95, 96 + 3 * 64 - 1] {
            let mut index = index_account();
            index.data.truncate(len);
            assert!(index_trader_state_range(&index, &FIRST_KEY).is_err());
        }
    }
}
