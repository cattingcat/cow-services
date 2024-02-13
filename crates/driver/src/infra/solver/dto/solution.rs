use {
    crate::{
        domain::{competition::{self, order}, eth, liquidity},
        infra::Solver,
        util::serialize,
    },
    itertools::Itertools,
    serde::{Deserialize, Serialize},
    serde_with::serde_as,
    std::collections::HashMap,
};

impl Solutions {
    pub fn into_domain(
        self,
        auction: &competition::Auction,
        liquidity: &[liquidity::Liquidity],
        weth: eth::WethAddress,
        solver: Solver,
        rank_by_surplus_date: Option<chrono::DateTime<chrono::Utc>>,
    ) -> Result<Vec<competition::Solution>, super::Error> {
        self.solutions
            .into_iter()
            .map(|solution| {
                competition::Solution::new(
                    solution.id.into(),
                    solution
                        .trades
                        .into_iter()
                        .map(|trade| match trade {
                            Trade::Fulfillment(fulfillment) => {
                                let order = auction
                                    .orders()
                                    .iter()
                                    .find(|order| order.uid == fulfillment.order)
                                    // TODO this error should reference the UID
                                    .ok_or(super::Error(
                                        "invalid order UID specified in fulfillment".to_owned()
                                    ))?
                                    .clone();

                                competition::solution::trade::Fulfillment::new(
                                    order,
                                    fulfillment.executed_amount.into(),
                                    match fulfillment.fee {
                                        Some(fee) => competition::solution::trade::Fee::Dynamic(
                                            competition::order::SellAmount(fee),
                                        ),
                                        None => competition::solution::trade::Fee::Static,
                                    },
                                )
                                .map(competition::solution::Trade::Fulfillment)
                                .map_err(|err| super::Error(format!("invalid fulfillment: {err}")))
                            }
                            Trade::Jit(jit) => Ok(competition::solution::Trade::Jit(
                                competition::solution::trade::Jit::new(
                                    competition::order::Jit {
                                        sell: eth::Asset {
                                            amount: jit.order.sell_amount.into(),
                                            token: jit.order.sell_token.into(),
                                        },
                                        buy: eth::Asset {
                                            amount: jit.order.buy_amount.into(),
                                            token: jit.order.buy_token.into(),
                                        },
                                        fee: jit.order.fee_amount.into(),
                                        receiver: jit.order.receiver.into(),
                                        valid_to: jit.order.valid_to.into(),
                                        app_data: jit.order.app_data.into(),
                                        side: match jit.order.kind {
                                            Kind::Sell => competition::order::Side::Sell,
                                            Kind::Buy => competition::order::Side::Buy,
                                        },
                                        partially_fillable: jit.order.partially_fillable,
                                        sell_token_balance: match jit.order.sell_token_balance {
                                            SellTokenBalance::Erc20 => {
                                                competition::order::SellTokenBalance::Erc20
                                            }
                                            SellTokenBalance::Internal => {
                                                competition::order::SellTokenBalance::Internal
                                            }
                                            SellTokenBalance::External => {
                                                competition::order::SellTokenBalance::External
                                            }
                                        },
                                        buy_token_balance: match jit.order.buy_token_balance {
                                            BuyTokenBalance::Erc20 => {
                                                competition::order::BuyTokenBalance::Erc20
                                            }
                                            BuyTokenBalance::Internal => {
                                                competition::order::BuyTokenBalance::Internal
                                            }
                                        },
                                        signature: competition::order::Signature {
                                            scheme: match jit.order.signing_scheme {
                                                SigningScheme::Eip712 => {
                                                    competition::order::signature::Scheme::Eip712
                                                }
                                                SigningScheme::EthSign => {
                                                    competition::order::signature::Scheme::EthSign
                                                }
                                                SigningScheme::PreSign => {
                                                    competition::order::signature::Scheme::PreSign
                                                }
                                                SigningScheme::Eip1271 => {
                                                    competition::order::signature::Scheme::Eip1271
                                                }
                                            },
                                            data: jit.order.signature.into(),
                                            signer: solver.address(),
                                        },
                                    },
                                    jit.executed_amount.into(),
                                )
                                .map_err(|err| super::Error(format!("invalid JIT trade: {err}")))?,
                            )),
                        })
                        .try_collect()?,
                    solution
                        .prices
                        .into_iter()
                        .map(|(address, price)| (address.into(), price))
                        .collect(),
                    solution
                        .interactions
                        .into_iter()
                        .map(|interaction| match interaction {
                            Interaction::Custom(interaction) => {
                                Ok(competition::solution::Interaction::Custom(
                                    competition::solution::interaction::Custom {
                                        target: interaction.target.into(),
                                        value: interaction.value.into(),
                                        call_data: interaction.call_data.into(),
                                        allowances: interaction
                                            .allowances
                                            .into_iter()
                                            .map(|allowance| {
                                                eth::Allowance {
                                                    token: allowance.token.into(),
                                                    spender: allowance.spender.into(),
                                                    amount: allowance.amount,
                                                }
                                                .into()
                                            })
                                            .collect(),
                                        inputs: interaction
                                            .inputs
                                            .into_iter()
                                            .map(|input| eth::Asset {
                                                amount: input.amount.into(),
                                                token: input.token.into(),
                                            })
                                            .collect(),
                                        outputs: interaction
                                            .outputs
                                            .into_iter()
                                            .map(|input| eth::Asset {
                                                amount: input.amount.into(),
                                                token: input.token.into(),
                                            })
                                            .collect(),
                                        internalize: interaction.internalize,
                                    },
                                ))
                            }
                            Interaction::Liquidity(interaction) => {
                                let liquidity = liquidity
                                    .iter()
                                    .find(|liquidity| liquidity.id == interaction.id)
                                    .ok_or(super::Error(
                                        "invalid liquidity ID specified in interaction".to_owned(),
                                    ))?
                                    .to_owned();
                                Ok(competition::solution::Interaction::Liquidity(
                                    competition::solution::interaction::Liquidity {
                                        liquidity,
                                        input: eth::Asset {
                                            amount: interaction.input_amount.into(),
                                            token: interaction.input_token.into(),
                                        },
                                        output: eth::Asset {
                                            amount: interaction.output_amount.into(),
                                            token: interaction.output_token.into(),
                                        },
                                        internalize: interaction.internalize,
                                    },
                                ))
                            }
                        })
                        .try_collect()?,
                    solver.clone(),
                    match rank_by_surplus_date
                        .is_some_and(|date| auction.deadline().driver() > date)
                    {
                        true => competition::solution::SolverScore::Surplus,
                        false => match solution.score {
                            Score::Solver { score } => {
                                competition::solution::SolverScore::Solver(score)
                            }
                            Score::RiskAdjusted {
                                success_probability,
                            } => competition::solution::SolverScore::RiskAdjusted(
                                success_probability,
                            ),
                        },
                    },
                    weth,
                    solution.gas.map(|gas| eth::Gas(gas.into())),
                )
                .map_err(|err| match err {
                    competition::solution::error::Solution::InvalidClearingPrices => {
                        super::Error("invalid clearing prices".to_owned())
                    }
                    competition::solution::error::Solution::ProtocolFee(err) => {
                        super::Error(format!("could not incorporate protocol fee: {err}"))
                    }
                })
            })
            .collect()
    }
}

#[serde_as]
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Solutions {
    pub solutions: Vec<Solution>,
}

#[serde_as]
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Solution {
    pub id: u64,
    #[serde_as(as = "HashMap<_, serialize::U256>")]
    pub prices: HashMap<eth::H160, eth::U256>,
    pub trades: Vec<Trade>,
    pub interactions: Vec<Interaction>,
    pub score: Score,
    pub gas: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase", deny_unknown_fields)]
pub enum Trade {
    Fulfillment(Fulfillment),
    Jit(JitTrade),
}

#[serde_as]
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Fulfillment {
    #[serde_as(as = "serialize::Hex")]
    pub order: [u8; order::UID_LEN],
    #[serde_as(as = "serialize::U256")]
    pub executed_amount: eth::U256,
    #[serde_as(as = "Option<serialize::U256>")]
    pub fee: Option<eth::U256>,
}

#[serde_as]
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct JitTrade {
    pub order: JitOrder,
    #[serde_as(as = "serialize::U256")]
    pub executed_amount: eth::U256,
}

#[serde_as]
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct JitOrder {
    pub sell_token: eth::H160,
    pub buy_token: eth::H160,
    pub receiver: eth::H160,
    #[serde_as(as = "serialize::U256")]
    pub sell_amount: eth::U256,
    #[serde_as(as = "serialize::U256")]
    pub buy_amount: eth::U256,
    pub valid_to: u32,
    #[serde_as(as = "serialize::Hex")]
    pub app_data: [u8; order::APP_DATA_LEN],
    #[serde_as(as = "serialize::U256")]
    pub fee_amount: eth::U256,
    pub kind: Kind,
    pub partially_fillable: bool,
    pub sell_token_balance: SellTokenBalance,
    pub buy_token_balance: BuyTokenBalance,
    pub signing_scheme: SigningScheme,
    #[serde_as(as = "serialize::Hex")]
    pub signature: Vec<u8>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub enum Kind {
    Sell,
    Buy,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase", deny_unknown_fields)]
pub enum Interaction {
    Liquidity(LiquidityInteraction),
    Custom(CustomInteraction),
}

#[serde_as]
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LiquidityInteraction {
    pub internalize: bool,
    #[serde_as(as = "serde_with::DisplayFromStr")]
    pub id: usize,
    pub input_token: eth::H160,
    pub output_token: eth::H160,
    #[serde_as(as = "serialize::U256")]
    pub input_amount: eth::U256,
    #[serde_as(as = "serialize::U256")]
    pub output_amount: eth::U256,
}

#[serde_as]
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CustomInteraction {
    pub internalize: bool,
    pub target: eth::H160,
    #[serde_as(as = "serialize::U256")]
    pub value: eth::U256,
    #[serde_as(as = "serialize::Hex")]
    pub call_data: Vec<u8>,
    pub allowances: Vec<Allowance>,
    pub inputs: Vec<Asset>,
    pub outputs: Vec<Asset>,
}

#[serde_as]
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Asset {
    pub token: eth::H160,
    #[serde_as(as = "serialize::U256")]
    pub amount: eth::U256,
}

#[serde_as]
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Allowance {
    pub token: eth::H160,
    pub spender: eth::H160,
    #[serde_as(as = "serialize::U256")]
    pub amount: eth::U256,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub enum SellTokenBalance {
    #[default]
    Erc20,
    Internal,
    External,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub enum BuyTokenBalance {
    #[default]
    Erc20,
    Internal,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub enum SigningScheme {
    Eip712,
    EthSign,
    PreSign,
    Eip1271,
}

#[serde_as]
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields, tag = "kind")]
pub enum Score {
    Solver {
        #[serde_as(as = "serialize::U256")]
        score: eth::U256,
    },
    #[serde(rename_all = "camelCase")]
    RiskAdjusted { success_probability: f64 },
}
