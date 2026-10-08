use std::{collections::HashSet, ops::Range};

use bytemuck::{Pod, Zeroable};
use phoenix_rise_accounts::{
    PhoenixAccount,
    multi_arena::MultiArenaHeader,
    trader::{Trader, TraderHeader},
};
use solana_account::Account;
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
    market::{MarketContext, market_books},
    state_builder::{
        Exchange, PHOENIX_GLOBAL_CONFIG, PHOENIX_PROGRAM_ID, Sandbox, hydrate, local_account,
        log_authority, phoenix_instruction, run_instructions,
    },
};
use crate::{
    error::{SurfpoolError, SurfpoolResult},
    surfnet::{remote::SurfnetRemoteClient, svm::SurfnetSvm},
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

/// The writes of a withdrawal the trader sends itself: `quote_lots` leave its collateral for its
/// wallet's token account, paid out of the global vault, so what remains stays backed. Phoenix
/// refuses more than the trader's margin leaves free.
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
    let mint = if tokens.program == spl_token_interface::ID {
        spl_token_interface::instruction::mint_to(
            &tokens.program,
            &tokens.mint,
            &tokens.wallet_account,
            &mint_authority,
            &[],
            quote_lots,
        )
    } else {
        spl_token_2022_interface::instruction::mint_to(
            &tokens.program,
            &tokens.mint,
            &tokens.wallet_account,
            &mint_authority,
            &[],
            quote_lots,
        )
    }
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
/// The most steps a price search takes.
const SEARCH_STEPS: usize = 40;

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
        let view = bytemuck::try_pod_read_unaligned::<Self>(data).map_err(|e| {
            SurfpoolError::internal(format!(
                "Hawkeye's margin view returned {} bytes: {e:?}",
                data.len()
            ))
        })?;
        if view.magic != MARGIN_VIEW_MAGIC {
            return Err(SurfpoolError::internal(format!(
                "Hawkeye's margin view returned magic {:#x}",
                view.magic
            )));
        }
        Ok(view)
    }

    /// Effective collateral at or below half the maintenance margin: liquidatable, and still far
    /// from the underwater state where the program refuses a market-order liquidation.
    fn reached(&self) -> bool {
        self.effective_collateral_quote_lots <= (self.maintenance_margin_quote_lots / 2) as i64
    }
}

/// A trader prepared for `liquidate_via_market_order`, and where the market was moved for it.
pub struct LiquidationReady {
    pub writes: Vec<(Pubkey, Account)>,
    pub target_ticks: u64,
    pub margin: MarginView,
    /// The liquidation that went through on a copy of the prepared state.
    pub liquidation: Instruction,
}

/// The writes that leave `trader` ready for `liquidate_via_market_order` on `symbol`. Its
/// resting orders on every market it holds are cancelled, since the program refuses to liquidate
/// a trader with risk-increasing orders anywhere, and the market moves, splines and book included, to the price where effective collateral
/// is half the maintenance margin. A liquidation is tried on a copy of the result, and nothing is
/// returned unless it goes through.
pub async fn prepare_liquidation(
    svm: &mut SurfnetSvm,
    remote_ctx: &Option<(SurfnetRemoteClient, CommitmentConfig)>,
    trader: Pubkey,
    symbol: &str,
) -> SurfpoolResult<LiquidationReady> {
    let mut context = MarketContext::load(svm, remote_ctx, symbol).await?;
    hydrate(svm, remote_ctx, &[trader, HAWKEYE_PROGRAM_ID]).await?;
    let account = local_account(svm, &trader)?;
    let cancels = cancel_everywhere(svm, remote_ctx, &context.exchange, trader, &account).await?;

    // The margin reads only the mark, so the search moves the oracles alone. Moves come a slot
    // after the cancels (see `Sandbox::next_slot`), and the cancels come first so the uncross
    // cannot fill the trader's own orders.
    let mut search = Sandbox::new(svm);
    search.run(&cancels)?;
    search.next_slot();
    let start = margin(&mut search, &context.exchange, &trader)?;
    let mark = context.market.mark_ticks;
    let probe = mark - (mark / 100).max(1);
    search.run(&context.oracle_reports(probe)?)?;
    let after_drop = margin(&mut search, &context.exchange, &trader)?;
    let long = match after_drop
        .effective_collateral_quote_lots
        .cmp(&start.effective_collateral_quote_lots)
    {
        std::cmp::Ordering::Less => true,
        std::cmp::Ordering::Greater => false,
        std::cmp::Ordering::Equal => {
            return Err(SurfpoolError::internal(format!(
                "{trader} holds no {symbol} position: moving its price changes nothing"
            )));
        }
    };

    let (mut healthy, mut reached) = (mark, if long { 1 } else { u64::from(u32::MAX) });
    for _ in 0..SEARCH_STEPS {
        if healthy.abs_diff(reached) <= 1 {
            break;
        }
        let candidate = midpoint(healthy, reached);
        search.run(&context.oracle_reports(candidate)?)?;
        if margin(&mut search, &context.exchange, &trader)?.reached() {
            reached = candidate;
        } else {
            healthy = candidate;
        }
    }

    let mut prepared = Sandbox::new(svm);
    prepared.run(&cancels)?;
    prepared.next_slot();
    prepared.run(&context.move_to(reached)?)?;
    let view = margin(&mut prepared, &context.exchange, &trader)?;
    if view.is_liquidatable != 1 || view.effective_collateral_quote_lots <= 0 {
        return Err(SurfpoolError::internal(format!(
            "{trader} did not land between liquidatable and underwater at {reached} ticks: \
             effective collateral {}, maintenance {}",
            view.effective_collateral_quote_lots, view.maintenance_margin_quote_lots
        )));
    }

    let liquidation = liquidation(&context, trader, long, reached)?;
    let mut trial = prepared.trial();
    trial.run(std::slice::from_ref(&liquidation)).map_err(|e| {
        SurfpoolError::internal(format!("a liquidation of {trader} would still fail: {e}"))
    })?;

    Ok(LiquidationReady {
        writes: prepared.writes()?,
        target_ticks: reached,
        margin: view,
        liquidation,
    })
}

/// The most position holders a cascade examines.
const MAX_CASCADE_CANDIDATES: usize = 24;

/// Traders a cascade leaves liquidatable at one price, and where the market was moved for them.
pub struct CascadeReady {
    pub writes: Vec<(Pubkey, Account)>,
    pub target_ticks: u64,
    /// Each trader with the liquidation that went through for it, in this order, on a copy of
    /// the prepared state.
    pub liquidations: Vec<(Pubkey, Instruction)>,
}

/// The writes that leave every trader holding long (or short) positions in `symbol` that one
/// price can make liquidatable ready for `liquidate_via_market_order`, in turn. Every holder's band
/// runs from where it turns liquidatable to where it goes underwater and Phoenix refuses a
/// market-order liquidation; the market moves, splines and book included, to the price inside
/// the most bands. The liquidations run one after another on a copy first; a trader whose
/// liquidation would fail is left out.
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
    let mut holders = Vec::new();
    for trader in listed {
        let account = local_account(svm, &trader)?;
        let Ok(view) = Trader::try_from_account_bytes(&account.data) else {
            continue;
        };
        let lots = view
            .positions()
            .find(|(asset, _)| *asset == context.market.asset_id)
            .map(|(_, position)| position.base_lot_position().as_inner())
            .unwrap_or(0);
        if (long && lots > 0) || (!long && lots < 0) {
            let Ok(cancels) =
                cancel_everywhere(svm, remote_ctx, &context.exchange, trader, &account).await
            else {
                continue;
            };
            holders.push((trader, cancels));
        }
        if holders.len() == MAX_CASCADE_CANDIDATES {
            break;
        }
    }

    let mark = context.market.mark_ticks;
    let far = if long {
        1
    } else {
        mark.saturating_mul(10).min(u64::from(u32::MAX))
    };
    let precision = (mark / 2000).max(1);
    let mut search = Sandbox::new(svm);
    let mut bands = Vec::new();
    for (trader, cancels) in &holders {
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
    for _ in 0..2 {
        let mut prepared = Sandbox::new(svm);
        for (trader, cancels) in &holders {
            if chosen.contains(trader) {
                prepared.run(cancels)?;
            }
        }
        prepared.next_slot();
        prepared.run(&context.move_to(target)?)?;
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
        if chosen.is_empty() {
            break;
        }
    }
    Err(SurfpoolError::internal(format!(
        "no liquidation of a {symbol} holder goes through at {target} ticks"
    )))
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
    search.run(&context.oracle_reports(far)?)?;
    if !reached(&margin(search, &context.exchange, trader)?) {
        return Ok(None);
    }
    let (mut healthy, mut hit) = (context.market.mark_ticks, far);
    while healthy.abs_diff(hit) > precision {
        let candidate = midpoint(healthy, hit);
        search.run(&context.oracle_reports(candidate)?)?;
        if reached(&margin(search, &context.exchange, trader)?) {
            hit = candidate;
        } else {
            healthy = candidate;
        }
    }
    Ok(Some(hit))
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

/// The trader cancelling its resting orders on every market it holds a position record in:
/// Phoenix refuses to liquidate a trader with risk-increasing orders on any market.
async fn cancel_everywhere(
    svm: &mut SurfnetSvm,
    remote_ctx: &Option<(SurfnetRemoteClient, CommitmentConfig)>,
    exchange: &Exchange,
    trader: Pubkey,
    account: &Account,
) -> SurfpoolResult<Vec<Instruction>> {
    let view = Trader::try_from_account_bytes(&account.data).map_err(|e| {
        SurfpoolError::invalid_account_data(trader, "Expected a Phoenix Trader", Some(e))
    })?;
    let assets: Vec<u64> = view.positions().map(|(asset, _)| asset).collect();
    let authority = Pubkey::new_from_array(view.header().authority);
    let map = local_account(svm, &exchange.perp_asset_map)?;
    let books = market_books(&exchange.perp_asset_map, &map.data, &assets)?;
    let book_accounts: Vec<Pubkey> = books
        .iter()
        .flat_map(|(book, splines)| [*book, *splines])
        .collect();
    hydrate(svm, remote_ctx, &book_accounts).await?;
    books
        .into_iter()
        .map(|book| cancel_all(exchange, authority, trader, book))
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
    };
    phoenix_instruction(
        "LiquidateViaMarketOrder",
        &[
            ("phoenixProgram", PHOENIX_PROGRAM_ID),
            ("phoenixLogAuthority", log_authority()),
            ("globalConfiguration", PHOENIX_GLOBAL_CONFIG),
            ("liquidatorWallet", Pubkey::new_unique()),
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
    let accounts = [
        PHOENIX_PROGRAM_ID,
        PHOENIX_GLOBAL_CONFIG,
        exchange.global_trader_index,
        exchange.active_trader_buffer,
        exchange.perp_asset_map,
        *trader,
    ]
    .iter()
    .map(|address| AccountMeta::new_readonly(*address, false))
    .collect();
    let data = sandbox.view(&Instruction {
        program_id: HAWKEYE_PROGRAM_ID,
        accounts,
        data: VIEW_MARGIN.to_vec(),
    })?;
    MarginView::from_return_data(&data)
}

/// Every hot Trader the GlobalTraderIndex tree reaches, paired with the byte range of its
/// TraderState record. The walk starts at the root, so freed nodes, which keep stale keys, flags
/// and collateral, are skipped. A key reached twice makes the tree invalid.
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

    #[test]
    fn index_listing_returns_only_reachable_records() {
        let index = index_account();
        let mut listed = index_trader_state_ranges(&index).unwrap();
        listed.sort_by_key(|(_, range)| range.start);
        assert_eq!(
            listed,
            vec![
                (Pubkey::new_from_array(FIRST_KEY), 144..160),
                (Pubkey::new_from_array(SECOND_KEY), 208..224),
            ],
            "slot 2 is a freed duplicate of FIRST_KEY, unreachable from the root and skipped"
        );

        let mut duplicated = index_account();
        duplicated.data[112..144].copy_from_slice(&SECOND_KEY);
        assert!(
            index_trader_state_ranges(&duplicated).is_err(),
            "a key reached twice"
        );
    }
}
