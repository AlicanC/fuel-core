use crate::database::Database;
use crate::schema::{
    scalars::{BlockId, U64},
    tx::types::Transaction,
};
use crate::{
    database::KvStoreError,
    model::fuel_block::{BlockHeight, FuelBlockDb},
    state::IterDirection,
};
use async_graphql::{
    connection::{query, Connection, Edge, EmptyFields},
    Context, Object,
};
use chrono::{DateTime, Utc};
use fuel_storage::Storage;
use itertools::Itertools;
use std::borrow::Cow;
use std::convert::TryInto;

use super::scalars::Address;

pub struct Block(pub(crate) FuelBlockDb);

#[Object]
impl Block {
    async fn id(&self) -> BlockId {
        self.0.id().into()
    }

    async fn height(&self) -> U64 {
        self.0.headers.height.into()
    }

    async fn transactions(&self, ctx: &Context<'_>) -> async_graphql::Result<Vec<Transaction>> {
        let db = ctx.data_unchecked::<Database>().clone();
        self.0
            .transactions
            .iter()
            .map(|tx_id| {
                Ok(Transaction(
                    Storage::<fuel_types::Bytes32, fuel_tx::Transaction>::get(&db, tx_id)
                        .and_then(|v| v.ok_or(KvStoreError::NotFound))?
                        .into_owned(),
                ))
            })
            .collect()
    }

    async fn time(&self) -> DateTime<Utc> {
        self.0.headers.time
    }

    async fn producer(&self) -> Address {
        self.0.headers.producer.into()
    }
}

#[derive(Default)]
pub struct BlockQuery;

#[Object]
impl BlockQuery {
    async fn block(
        &self,
        ctx: &Context<'_>,
        #[graphql(desc = "ID of the block")] id: Option<BlockId>,
        #[graphql(desc = "Height of the block")] height: Option<U64>,
    ) -> async_graphql::Result<Option<Block>> {
        let db = ctx.data_unchecked::<Database>();
        let id = match (id, height) {
            (Some(_), Some(_)) => {
                return Err(async_graphql::Error::new(
                    "Can't provide both an id and a height",
                ))
            }
            (Some(id), None) => id.into(),
            (None, Some(height)) => {
                let height: u64 = height.into();
                if height == 0 {
                    return Err(async_graphql::Error::new(
                        "Genesis block isn't implemented yet",
                    ));
                } else {
                    db.get_block_id(height.try_into()?)?
                        .ok_or("Block height non-existent")?
                }
            }
            (None, None) => return Err(async_graphql::Error::new("Missing either id or height")),
        };

        let block = Storage::<fuel_types::Bytes32, FuelBlockDb>::get(db, &id)?
            .map(|b| Block(b.into_owned()));
        Ok(block)
    }

    async fn blocks(
        &self,
        ctx: &Context<'_>,
        first: Option<i32>,
        after: Option<String>,
        last: Option<i32>,
        before: Option<String>,
    ) -> async_graphql::Result<Connection<usize, Block, EmptyFields, EmptyFields>> {
        let db = ctx.data_unchecked::<Database>().clone();

        query(
            after,
            before,
            first,
            last,
            |after: Option<usize>, before: Option<usize>, first, last| async move {
                let (records_to_fetch, direction) = if let Some(first) = first {
                    (first, IterDirection::Forward)
                } else if let Some(last) = last {
                    (last, IterDirection::Reverse)
                } else {
                    (0, IterDirection::Forward)
                };

                let start;
                let end;

                if direction == IterDirection::Forward {
                    start = after;
                    end = before;
                } else {
                    start = before;
                    end = after;
                }

                let mut blocks = db.all_block_ids(start.map(Into::into), Some(direction));
                let mut started = None;
                if start.is_some() {
                    // skip initial result
                    started = blocks.next();
                }

                // take desired amount of results
                let blocks = blocks
                    .take_while(|r| {
                        if let (Ok(b), Some(end)) = (r, end) {
                            if b.0 == end.into() {
                                return false;
                            }
                        }
                        true
                    })
                    .take(records_to_fetch);
                let mut blocks: Vec<(BlockHeight, fuel_types::Bytes32)> = blocks.try_collect()?;
                if direction == IterDirection::Forward {
                    blocks.reverse();
                }

                // TODO: do a batch get instead
                let blocks: Vec<Cow<FuelBlockDb>> = blocks
                    .iter()
                    .map(|(_, id)| {
                        Storage::<fuel_types::Bytes32, FuelBlockDb>::get(&db, id)
                            .transpose()
                            .ok_or(KvStoreError::NotFound)?
                    })
                    .try_collect()?;

                let mut connection =
                    Connection::new(started.is_some(), records_to_fetch <= blocks.len());
                connection.append(
                    blocks
                        .into_iter()
                        .map(|item| Edge::new(item.headers.height.to_usize(), item.into_owned())),
                );
                Ok(connection)
            },
        )
        .await
        .map(|conn| conn.map_node(Block))
    }
}
