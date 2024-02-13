use {
    crate::{
        domain::{
            competition,
            competition::{
                order,
                order::{FeePolicy, Side},
            },
            eth,
            liquidity,
        },
        util::{
            conv::{rational_to_big_decimal, u256::U256Ext},
            serialize,
        },
    },
    indexmap::IndexMap,
    serde::{Deserialize, Serialize},
    serde_with::serde_as,
    std::collections::{BTreeMap, HashMap},
};

impl Auction {
    pub fn new(
        auction: &competition::Auction,
        liquidity: &[liquidity::Liquidity],
        weth: eth::WethAddress,
    ) -> Self {
        let mut tokens: HashMap<eth::H160, _> = auction
            .tokens()
            .iter()
            .map(|token| {
                (
                    token.address.into(),
                    Token {
                        decimals: token.decimals,
                        symbol: token.symbol.clone(),
                        reference_price: token.price.map(Into::into),
                        available_balance: token.available_balance,
                        trusted: token.trusted,
                    },
                )
            })
            .collect();

        // Make sure that we have at least empty entries for all tokens for
        // which we are providing liquidity.
        for token in liquidity
            .iter()
            .flat_map(|liquidity| match &liquidity.kind {
                liquidity::Kind::UniswapV2(pool) => pool.reserves.iter().map(|r| r.token).collect(),
                liquidity::Kind::UniswapV3(pool) => vec![pool.tokens.get().0, pool.tokens.get().1],
                liquidity::Kind::BalancerV2Stable(pool) => pool.reserves.tokens().collect(),
                liquidity::Kind::BalancerV2Weighted(pool) => pool.reserves.tokens().collect(),
                liquidity::Kind::Swapr(pool) => {
                    pool.base.reserves.iter().map(|r| r.token).collect()
                }
                liquidity::Kind::ZeroEx(_) => todo!(),
            })
        {
            tokens.entry(token.into()).or_insert_with(Default::default);
        }

        Self {
            id: auction.id().as_ref().map(ToString::to_string),
            orders: auction
                .orders()
                .iter()
                .map(|order| {
                    let mut available = order.available(weth);
                    // Solvers are unaware of the protocol fees. In case of volume based fees,
                    // fee withheld by driver might be higher than the surplus of the solution. This
                    // would lead to violating limit prices when driver tries to withhold the
                    // volume based fee. To avoid this, we artifically adjust the order limit
                    // amounts (make then worse) before sending to solvers, to force solvers to only
                    // submit solutions with enough surplus to cover the fee.
                    //
                    // https://github.com/cowprotocol/services/issues/2440
                    if let Some(FeePolicy::Volume { factor }) = order.protocol_fees.first() {
                        match order.side {
                            Side::Buy => {
                                // reduce sell amount by factor
                                available.sell.amount = available
                                    .sell
                                    .amount
                                    .apply_factor(1.0 / (1.0 + factor))
                                    .unwrap_or_default();
                            }
                            Side::Sell => {
                                // increase buy amount by factor
                                available.buy.amount = available
                                    .buy
                                    .amount
                                    .apply_factor(1.0 / (1.0 - factor))
                                    .unwrap_or_default();
                            }
                        }
                    }
                    Order {
                        uid: order.uid.into(),
                        sell_token: available.sell.token.into(),
                        buy_token: available.buy.token.into(),
                        sell_amount: available.sell.amount.into(),
                        buy_amount: available.buy.amount.into(),
                        fee_amount: available.user_fee.into(),
                        kind: match order.side {
                            competition::order::Side::Buy => Kind::Buy,
                            competition::order::Side::Sell => Kind::Sell,
                        },
                        partially_fillable: order.is_partial(),
                        class: match order.kind {
                            competition::order::Kind::Market => Class::Market,
                            competition::order::Kind::Limit { .. } => Class::Limit,
                            competition::order::Kind::Liquidity => Class::Liquidity,
                        },
                    }
                })
                .collect(),
            liquidity: liquidity
                .iter()
                .map(|liquidity| match &liquidity.kind {
                    liquidity::Kind::UniswapV2(pool) => {
                        Liquidity::ConstantProduct(ConstantProductPool {
                            id: liquidity.id.into(),
                            address: pool.address.into(),
                            router: pool.router.into(),
                            gas_estimate: liquidity.gas.into(),
                            tokens: pool
                                .reserves
                                .iter()
                                .map(|asset| {
                                    (
                                        asset.token.into(),
                                        ConstantProductReserve {
                                            balance: asset.amount.into(),
                                        },
                                    )
                                })
                                .collect(),
                            fee: bigdecimal::BigDecimal::new(3.into(), 3),
                        })
                    }
                    liquidity::Kind::UniswapV3(pool) => {
                        Liquidity::ConcentratedLiquidity(ConcentratedLiquidityPool {
                            id: liquidity.id.into(),
                            address: pool.address.0,
                            router: pool.router.into(),
                            gas_estimate: liquidity.gas.0,
                            tokens: vec![pool.tokens.get().0.into(), pool.tokens.get().1.into()],
                            sqrt_price: pool.sqrt_price.0,
                            liquidity: pool.liquidity.0,
                            tick: pool.tick.0,
                            liquidity_net: pool
                                .liquidity_net
                                .iter()
                                .map(|(key, value)| (key.0, value.0))
                                .collect(),
                            fee: rational_to_big_decimal(&pool.fee.0),
                        })
                    }
                    liquidity::Kind::BalancerV2Stable(pool) => Liquidity::Stable(StablePool {
                        id: liquidity.id.into(),
                        address: pool.id.address().into(),
                        balancer_pool_id: pool.id.into(),
                        gas_estimate: liquidity.gas.into(),
                        tokens: pool
                            .reserves
                            .iter()
                            .map(|r| {
                                (
                                    r.asset.token.into(),
                                    StableReserve {
                                        balance: r.asset.amount.into(),
                                        scaling_factor: scaling_factor_to_decimal(r.scale),
                                    },
                                )
                            })
                            .collect(),
                        amplification_parameter: rational_to_big_decimal(&num::BigRational::new(
                            pool.amplification_parameter.factor().to_big_int(),
                            pool.amplification_parameter.precision().to_big_int(),
                        )),
                        fee: fee_to_decimal(pool.fee),
                    }),
                    liquidity::Kind::BalancerV2Weighted(pool) => {
                        Liquidity::WeightedProduct(WeightedProductPool {
                            id: liquidity.id.into(),
                            address: pool.id.address().into(),
                            balancer_pool_id: pool.id.into(),
                            gas_estimate: liquidity.gas.into(),
                            tokens: pool
                                .reserves
                                .iter()
                                .map(|r| {
                                    (
                                        r.asset.token.into(),
                                        WeightedProductReserve {
                                            balance: r.asset.amount.into(),
                                            scaling_factor: scaling_factor_to_decimal(r.scale),
                                            weight: weight_to_decimal(r.weight),
                                        },
                                    )
                                })
                                .collect(),
                            fee: fee_to_decimal(pool.fee),
                            version: match pool.version {
                                liquidity::balancer::v2::weighted::Version::V0 => {
                                    WeightedProductVersion::V0
                                }
                                liquidity::balancer::v2::weighted::Version::V3Plus => {
                                    WeightedProductVersion::V3Plus
                                }
                            },
                        })
                    }
                    liquidity::Kind::Swapr(pool) => {
                        Liquidity::ConstantProduct(ConstantProductPool {
                            id: liquidity.id.into(),
                            address: pool.base.address.into(),
                            router: pool.base.router.into(),
                            gas_estimate: liquidity.gas.into(),
                            tokens: pool
                                .base
                                .reserves
                                .iter()
                                .map(|asset| {
                                    (
                                        asset.token.into(),
                                        ConstantProductReserve {
                                            balance: asset.amount.into(),
                                        },
                                    )
                                })
                                .collect(),
                            fee: bigdecimal::BigDecimal::new(pool.fee.bps().into(), 4),
                        })
                    }
                    liquidity::Kind::ZeroEx(_) => todo!(),
                })
                .collect(),
            tokens,
            effective_gas_price: auction.gas_price().effective().into(),
            deadline: auction.deadline().solvers(),
        }
    }
}

#[serde_as]
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Auction {
    pub id: Option<String>,
    pub tokens: HashMap<eth::H160, Token>,
    pub orders: Vec<Order>,
    pub liquidity: Vec<Liquidity>,
    #[serde_as(as = "serialize::U256")]
    pub effective_gas_price: eth::U256,
    pub deadline: chrono::DateTime<chrono::Utc>,
}

#[serde_as]
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Order {
    #[serde_as(as = "serialize::Hex")]
    pub uid: [u8; order::UID_LEN],
    pub sell_token: eth::H160,
    pub buy_token: eth::H160,
    #[serde_as(as = "serialize::U256")]
    pub sell_amount: eth::U256,
    #[serde_as(as = "serialize::U256")]
    pub buy_amount: eth::U256,
    #[serde_as(as = "serialize::U256")]
    pub fee_amount: eth::U256,
    pub kind: Kind,
    pub partially_fillable: bool,
    pub class: Class,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Kind {
    Sell,
    Buy,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Class {
    Market,
    Limit,
    Liquidity,
}

#[serde_as]
#[derive(Default, Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Token {
    pub decimals: Option<u8>,
    pub symbol: Option<String>,
    #[serde_as(as = "Option<serialize::U256>")]
    pub reference_price: Option<eth::U256>,
    #[serde_as(as = "serialize::U256")]
    pub available_balance: eth::U256,
    pub trusted: bool,
}

// TODO Remove dead_code
#[allow(dead_code, clippy::enum_variant_names)]
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum Liquidity {
    ConstantProduct(ConstantProductPool),
    WeightedProduct(WeightedProductPool),
    Stable(StablePool),
    ConcentratedLiquidity(ConcentratedLiquidityPool),
    LimitOrder(ForeignLimitOrder),
}

#[serde_as]
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConstantProductPool {
    #[serde_as(as = "serde_with::DisplayFromStr")]
<<<<<<< HEAD
    id: usize,
    address: eth::H160,
    router: eth::H160,
=======
    pub id: usize,
    pub address: eth::H160,
>>>>>>> b364f9b0 (chore: export DTOs for solver)
    #[serde_as(as = "serialize::U256")]
    pub gas_estimate: eth::U256,
    pub tokens: BTreeMap<eth::H160, ConstantProductReserve>,
    #[serde_as(as = "serde_with::DisplayFromStr")]
    pub fee: bigdecimal::BigDecimal,
}

#[serde_as]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConstantProductReserve {
    #[serde_as(as = "serialize::U256")]
    pub balance: eth::U256,
}

#[serde_as]
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WeightedProductPool {
    #[serde_as(as = "serde_with::DisplayFromStr")]
<<<<<<< HEAD
    id: usize,
    address: eth::H160,
    balancer_pool_id: eth::H256,
=======
    pub id: usize,
    pub address: eth::H160,
>>>>>>> b364f9b0 (chore: export DTOs for solver)
    #[serde_as(as = "serialize::U256")]
    pub gas_estimate: eth::U256,
    pub tokens: IndexMap<eth::H160, WeightedProductReserve>,
    #[serde_as(as = "serde_with::DisplayFromStr")]
    pub fee: bigdecimal::BigDecimal,
    pub version: WeightedProductVersion,
}

#[serde_as]
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WeightedProductReserve {
    #[serde_as(as = "serialize::U256")]
    pub balance: eth::U256,
    #[serde_as(as = "serde_with::DisplayFromStr")]
    pub scaling_factor: bigdecimal::BigDecimal,
    #[serde_as(as = "serde_with::DisplayFromStr")]
    pub weight: bigdecimal::BigDecimal,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum WeightedProductVersion {
    V0,
    V3Plus,
}

#[serde_as]
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StablePool {
    #[serde_as(as = "serde_with::DisplayFromStr")]
<<<<<<< HEAD
    id: usize,
    address: eth::H160,
    balancer_pool_id: eth::H256,
=======
    pub id: usize,
    pub address: eth::H160,
>>>>>>> b364f9b0 (chore: export DTOs for solver)
    #[serde_as(as = "serialize::U256")]
    pub gas_estimate: eth::U256,
    pub tokens: IndexMap<eth::H160, StableReserve>,
    #[serde_as(as = "serde_with::DisplayFromStr")]
    pub amplification_parameter: bigdecimal::BigDecimal,
    #[serde_as(as = "serde_with::DisplayFromStr")]
    pub fee: bigdecimal::BigDecimal,
}

#[serde_as]
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StableReserve {
    #[serde_as(as = "serialize::U256")]
    pub balance: eth::U256,
    #[serde_as(as = "serde_with::DisplayFromStr")]
    pub scaling_factor: bigdecimal::BigDecimal,
}

#[serde_as]
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConcentratedLiquidityPool {
    #[serde_as(as = "serde_with::DisplayFromStr")]
<<<<<<< HEAD
    id: usize,
    address: eth::H160,
    router: eth::H160,
=======
    pub id: usize,
    pub address: eth::H160,
>>>>>>> b364f9b0 (chore: export DTOs for solver)
    #[serde_as(as = "serialize::U256")]
    pub gas_estimate: eth::U256,
    pub tokens: Vec<eth::H160>,
    #[serde_as(as = "serialize::U256")]
    pub sqrt_price: eth::U256,
    #[serde_as(as = "serde_with::DisplayFromStr")]
    pub liquidity: u128,
    pub tick: i32,
    #[serde_as(as = "BTreeMap<serde_with::DisplayFromStr, serde_with::DisplayFromStr>")]
    pub liquidity_net: BTreeMap<i32, i128>,
    #[serde_as(as = "serde_with::DisplayFromStr")]
    pub fee: bigdecimal::BigDecimal,
}

#[serde_as]
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ForeignLimitOrder {
    #[serde_as(as = "serde_with::DisplayFromStr")]
    pub id: usize,
    pub address: eth::H160,
    #[serde_as(as = "serialize::U256")]
    pub gas_estimate: eth::U256,
    #[serde_as(as = "serialize::Hex")]
    pub hash: [u8; 32],
    pub maker_token: eth::H160,
    pub taker_token: eth::H160,
    #[serde_as(as = "serialize::U256")]
    pub maker_amount: eth::U256,
    #[serde_as(as = "serialize::U256")]
    pub taker_amount: eth::U256,
    #[serde_as(as = "serialize::U256")]
    pub taker_token_fee_amount: eth::U256,
}

fn fee_to_decimal(fee: liquidity::balancer::v2::Fee) -> bigdecimal::BigDecimal {
    bigdecimal::BigDecimal::new(fee.as_raw().to_big_int(), 18)
}

fn weight_to_decimal(weight: liquidity::balancer::v2::weighted::Weight) -> bigdecimal::BigDecimal {
    bigdecimal::BigDecimal::new(weight.as_raw().to_big_int(), 18)
}

fn scaling_factor_to_decimal(
    scale: liquidity::balancer::v2::ScalingFactor,
) -> bigdecimal::BigDecimal {
    bigdecimal::BigDecimal::new(scale.as_raw().to_big_int(), 18)
}
