use crate::client::schema::{
    primitives::Address, primitives::DateTime, schema, BlockId, ConnectionArgs, PageInfo, U64,
};
use crate::client::PaginatedResult;

use super::tx::TransactionIdFragment;

#[derive(cynic::FragmentArguments, Debug)]
pub struct BlockByIdArgs {
    pub id: BlockId,
}

#[derive(cynic::QueryFragment, Debug)]
#[cynic(
    schema_path = "./assets/schema.sdl",
    graphql_type = "Query",
    argument_struct = "BlockByIdArgs"
)]
pub struct BlockByIdQuery {
    #[arguments(id = &args.id)]
    pub block: Option<Block>,
}

#[derive(cynic::QueryFragment, Debug)]
#[cynic(
    schema_path = "./assets/schema.sdl",
    graphql_type = "Query",
    argument_struct = "ConnectionArgs"
)]
pub struct BlocksQuery {
    #[arguments(after = &args.after, before = &args.before, first = &args.first, last = &args.last)]
    pub blocks: BlockConnection,
}

#[derive(cynic::QueryFragment, Debug)]
#[cynic(schema_path = "./assets/schema.sdl")]
pub struct BlockConnection {
    pub edges: Option<Vec<Option<BlockEdge>>>,
    pub page_info: PageInfo,
}

impl From<BlockConnection> for PaginatedResult<Block, String> {
    fn from(conn: BlockConnection) -> Self {
        PaginatedResult {
            cursor: conn.page_info.end_cursor,
            results: conn
                .edges
                .unwrap_or_default()
                .into_iter()
                .filter_map(|e| e.map(|e| e.node))
                .collect(),
        }
    }
}

#[derive(cynic::QueryFragment, Debug)]
#[cynic(schema_path = "./assets/schema.sdl")]
pub struct BlockEdge {
    pub cursor: String,
    pub node: Block,
}

#[derive(cynic::QueryFragment, Debug)]
#[cynic(schema_path = "./assets/schema.sdl")]
pub struct Block {
    pub height: U64,
    pub id: BlockId,
    pub time: DateTime,
    pub producer: Address,
    pub transactions: Vec<TransactionIdFragment>,
}

#[derive(cynic::QueryFragment, Debug)]
#[cynic(schema_path = "./assets/schema.sdl", graphql_type = "Block")]
pub struct BlockIdFragment {
    pub id: BlockId,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn block_by_id_query_gql_output() {
        use cynic::QueryBuilder;
        let operation = BlockByIdQuery::build(BlockByIdArgs {
            id: BlockId::default(),
        });
        insta::assert_snapshot!(operation.query)
    }

    #[test]
    fn blocks_connection_query_gql_output() {
        use cynic::QueryBuilder;
        let operation = BlocksQuery::build(ConnectionArgs {
            after: None,
            before: None,
            first: None,
            last: None,
        });
        insta::assert_snapshot!(operation.query)
    }
}
