use async_graphql::{EmptySubscription, MergedObject, Schema, SchemaBuilder};

pub mod block;
pub mod chain;
pub mod coin;
pub mod contract;
pub mod dap;
pub mod health;
pub mod scalars;
pub mod tx;

#[derive(MergedObject, Default)]
pub struct Query(
    dap::DapQuery,
    block::BlockQuery,
    chain::ChainQuery,
    tx::TxQuery,
    health::HealthQuery,
    coin::CoinQuery,
    contract::ContractQuery,
);

#[derive(MergedObject, Default)]
pub struct Mutation(dap::DapMutation, tx::TxMutation);

// Placeholder for when we need to add subscriptions
// #[derive(MergedSubscription, Default)]
// pub struct Subscription();

pub type CoreSchema = Schema<Query, Mutation, EmptySubscription>;

pub fn build_schema() -> SchemaBuilder<Query, Mutation, EmptySubscription> {
    Schema::build(
        Query::default(),
        Mutation::default(),
        EmptySubscription::default(),
    )
}
