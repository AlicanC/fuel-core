use crate::database::{Database, KvStoreError};
use crate::executor::{ExecutionMode, Executor};
use crate::model::fuel_block::{FuelBlock, FuelBlockHeader};
use crate::service::Config;
use chrono::{DateTime, Utc};
use fuel_core_interfaces::txpool::{TxPool as TxPoolTrait, TxPoolDb};
use fuel_storage::Storage;
use fuel_tx::{Bytes32, Receipt};
use fuel_txpool::{Config as TxPoolConfig, TxPoolService};
use fuel_vm::prelude::{ProgramState, Transaction};
use serde::{Deserialize, Serialize};
use std::error::Error as StdError;
use std::sync::Arc;
use thiserror::Error;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum TransactionStatus {
    Submitted {
        time: DateTime<Utc>,
    },
    Success {
        block_id: Bytes32,
        time: DateTime<Utc>,
        result: ProgramState,
    },
    Failed {
        block_id: Bytes32,
        time: DateTime<Utc>,
        reason: String,
        result: Option<ProgramState>,
    },
}

#[derive(Error, Debug)]
pub enum Error {
    #[error("unexpected database error {0:?}")]
    Database(Box<dyn StdError>),
    #[error("unexpected block execution error {0:?}")]
    Execution(crate::executor::Error),
    #[error("Tx is invalid, insertion failed {0:?}")]
    Other(#[from] anyhow::Error),
}

impl From<KvStoreError> for Error {
    fn from(e: KvStoreError) -> Self {
        Error::Database(Box::new(e))
    }
}

impl From<crate::state::Error> for Error {
    fn from(e: crate::state::Error) -> Self {
        Error::Database(Box::new(e))
    }
}

impl TxPoolDb for Database {}

/// Holds submitted transactions and attempts to propose blocks
pub struct TxPool {
    executor: Executor,
    db: Database,
    fuel_txpool: Box<dyn TxPoolTrait>,
}

impl TxPool {
    pub fn pool(&self) -> &dyn TxPoolTrait {
        self.fuel_txpool.as_ref()
    }

    pub fn new(database: Database, config: Config) -> Self {
        let executor = Executor {
            database: database.clone(),
            config,
        };
        let config = Arc::new(TxPoolConfig::default());
        TxPool {
            executor,
            db: database.clone(),
            fuel_txpool: Box::new(TxPoolService::new(
                Box::new(database) as Box<dyn TxPoolDb>,
                config,
            )),
        }
    }

    pub async fn submit_tx(&self, tx: Transaction) -> Result<Bytes32, Error> {
        let db = self.db.clone();

        let mut tx_to_exec = tx.clone();

        let includable_txs: Vec<Transaction>;

        if self.executor.config.utxo_validation {
            if tx_to_exec.metadata().is_none() {
                tx_to_exec.precompute_metadata();
            }

            self.fuel_txpool
                .insert(vec![Arc::new(tx_to_exec.clone())])
                .await
                .into_iter()
                .collect::<Result<Vec<_>, _>>()?;

            let includable_arc_txs = self.fuel_txpool.includable().await;

            includable_txs = includable_arc_txs
                .iter()
                .map(|arc| Transaction::clone(&*arc))
                .collect();

            for included_tx in includable_arc_txs {
                self.fuel_txpool.remove(&[included_tx.id()]).await;
            }
        } else {
            includable_txs = vec![tx];
        }

        let tx_id = tx_to_exec.id();

        // set status to submitted
        db.update_tx_status(
            &tx_id.clone(),
            TransactionStatus::Submitted { time: Utc::now() },
        )?;

        // setup and execute block
        let current_height = db.get_block_height()?.unwrap_or_default();
        let current_hash = db.get_block_id(current_height)?.unwrap_or_default();
        let new_block_height = current_height + 1u32.into();

        let mut block = FuelBlock {
            header: FuelBlockHeader {
                height: new_block_height,
                number: Default::default(),
                parent_hash: current_hash,
                time: Utc::now(),
                producer: Default::default(),
                transactions_root: Default::default(),
                // TODO: compute the current merkle root of all blocks
                prev_root: Default::default(),
            },
            transactions: includable_txs,
        };
        // immediately execute block
        self.executor
            .execute(&mut block, ExecutionMode::Production)
            .await
            .map_err(Error::Execution)?;
        Ok(tx_id)
    }

    pub async fn run_tx(&self, tx: Transaction) -> Result<Vec<Receipt>, Error> {
        let id = self.submit_tx(tx).await?;
        // note: we'll need to await tx completion once it's not instantaneous
        let db = &self.db;
        let receipts = Storage::<Bytes32, Vec<Receipt>>::get(db, &id)?.unwrap_or_default();
        Ok(receipts.into_owned())
    }
}
