#[cfg(feature = "rocksdb")]
use crate::database::columns::COLUMN_NUM;
use crate::database::transactional::DatabaseTransaction;
use crate::model::fuel_block::FuelBlockDb;
#[cfg(feature = "rocksdb")]
use crate::state::rocks_db::RocksDb;
use crate::state::{
    in_memory::memory_store::MemoryStore, ColumnId, DataSource, Error, IterDirection,
};
pub use fuel_core_interfaces::db::KvStoreError;
use fuel_storage::Storage;
use fuel_vm::prelude::{Address, Bytes32, InterpreterStorage};
use serde::{de::DeserializeOwned, Serialize};
#[cfg(feature = "rocksdb")]
use std::path::Path;
use std::{
    fmt::{self, Debug, Formatter},
    marker::Send,
    sync::Arc,
};
#[cfg(feature = "rocksdb")]
use tempfile::TempDir;

pub mod balances;
pub mod block;
pub mod code_root;
pub mod coin;
pub mod contracts;
pub mod metadata;
mod receipts;
pub mod state;
pub mod transaction;
pub mod transactional;

// Crude way to invalidate incompatible databases,
// can be used to perform migrations in the future.
pub const VERSION: u32 = 0;

pub mod columns {
    pub const METADATA: u32 = 0;
    pub const CONTRACTS: u32 = 1;
    pub const CONTRACTS_CODE_ROOT: u32 = 2;
    pub const CONTRACTS_STATE: u32 = 3;
    // Contract Id -> Utxo Id
    pub const CONTRACT_UTXO_ID: u32 = 4;
    pub const BALANCES: u32 = 5;
    pub const COIN: u32 = 6;
    // (owner, coin id) => true
    pub const OWNED_COINS: u32 = 7;
    pub const TRANSACTIONS: u32 = 8;
    // tx id -> current status
    pub const TRANSACTION_STATUS: u32 = 9;
    pub const TRANSACTIONS_BY_OWNER_BLOCK_IDX: u32 = 10;
    pub const RECEIPTS: u32 = 11;
    pub const BLOCKS: u32 = 12;
    // maps block id -> block hash
    pub const BLOCK_IDS: u32 = 13;

    // Number of columns
    #[cfg(feature = "rocksdb")]
    pub const COLUMN_NUM: u32 = 14;
}

#[derive(Clone, Debug)]
pub struct Database {
    data: DataSource,
    // used for RAII
    _drop: Arc<DropResources>,
}

trait DropFnTrait: FnOnce() {}
impl<F> DropFnTrait for F where F: FnOnce() {}
type DropFn = Box<dyn DropFnTrait>;

impl fmt::Debug for DropFn {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(f, "DropFn")
    }
}

#[derive(Debug, Default)]
struct DropResources {
    // move resources into this closure to have them dropped when db drops
    drop: Option<DropFn>,
}

impl<F: 'static + FnOnce()> From<F> for DropResources {
    fn from(closure: F) -> Self {
        Self {
            drop: Option::Some(Box::new(closure)),
        }
    }
}

impl Drop for DropResources {
    fn drop(&mut self) {
        if let Some(drop) = self.drop.take() {
            (drop)()
        }
    }
}

/*** SAFETY: we are safe to do it because DataSource is Send+Sync and there is nowhere it is overwritten
 * it is not Send+Sync by default because Storage insert fn takes &mut self
*/
unsafe impl Send for Database {}
unsafe impl Sync for Database {}

impl Database {
    #[cfg(feature = "rocksdb")]
    pub fn open(path: &Path) -> Result<Self, Error> {
        let db = RocksDb::open(path, COLUMN_NUM)?;

        Ok(Database {
            data: Arc::new(db),
            _drop: Default::default(),
        })
    }

    pub fn in_memory() -> Self {
        Self {
            data: Arc::new(MemoryStore::default()),
            _drop: Default::default(),
        }
    }

    fn insert<K: Into<Vec<u8>>, V: Serialize + DeserializeOwned>(
        &self,
        key: K,
        column: ColumnId,
        value: V,
    ) -> Result<Option<V>, Error> {
        let result = self.data.put(
            key.into(),
            column,
            bincode::serialize(&value).map_err(|_| Error::Codec)?,
        )?;
        if let Some(previous) = result {
            Ok(Some(
                bincode::deserialize(&previous).map_err(|_| Error::Codec)?,
            ))
        } else {
            Ok(None)
        }
    }

    fn remove<V: DeserializeOwned>(
        &self,
        key: &[u8],
        column: ColumnId,
    ) -> Result<Option<V>, Error> {
        self.data
            .delete(key, column)?
            .map(|val| bincode::deserialize(&val).map_err(|_| Error::Codec))
            .transpose()
    }

    fn get<V: DeserializeOwned>(&self, key: &[u8], column: ColumnId) -> Result<Option<V>, Error> {
        self.data
            .get(key, column)?
            .map(|val| bincode::deserialize(&val).map_err(|_| Error::Codec))
            .transpose()
    }

    fn exists(&self, key: &[u8], column: ColumnId) -> Result<bool, Error> {
        self.data.exists(key, column)
    }

    fn iter_all<K, V>(
        &self,
        column: ColumnId,
        prefix: Option<Vec<u8>>,
        start: Option<Vec<u8>>,
        direction: Option<IterDirection>,
    ) -> impl Iterator<Item = Result<(K, V), Error>> + '_
    where
        K: From<Vec<u8>>,
        V: DeserializeOwned,
    {
        self.data
            .iter_all(column, prefix, start, direction.unwrap_or_default())
            .map(|(key, value)| {
                let key = K::from(key);
                let value: V = bincode::deserialize(&value).map_err(|_| Error::Codec)?;
                Ok((key, value))
            })
    }

    pub fn transaction(&self) -> DatabaseTransaction {
        self.into()
    }
}

impl AsRef<Database> for Database {
    fn as_ref(&self) -> &Database {
        self
    }
}

/// Construct an ephemeral database
/// uses rocksdb when rocksdb features are enabled
/// uses in-memory when rocksdb features are disabled
impl Default for Database {
    fn default() -> Self {
        #[cfg(not(feature = "rocksdb"))]
        {
            Self {
                data: Arc::new(MemoryStore::default()),
                _drop: Default::default(),
            }
        }
        #[cfg(feature = "rocksdb")]
        {
            let tmp_dir = TempDir::new().unwrap();
            Self {
                data: Arc::new(RocksDb::open(tmp_dir.path(), columns::COLUMN_NUM).unwrap()),
                _drop: Arc::new(
                    {
                        move || {
                            // cleanup temp dir
                            drop(tmp_dir);
                        }
                    }
                    .into(),
                ),
            }
        }
    }
}

impl InterpreterStorage for Database {
    type DataError = Error;

    fn block_height(&self) -> Result<u32, Error> {
        let height = self.get_block_height()?.unwrap_or_default();
        Ok(height.into())
    }

    fn block_hash(&self, block_height: u32) -> Result<Bytes32, Error> {
        let hash = self.get_block_id(block_height.into())?.unwrap_or_default();
        Ok(hash)
    }

    fn coinbase(&self) -> Result<Address, Error> {
        let height = self.get_block_height()?.unwrap_or_default();
        let id = self.block_hash(height.into())?;
        let block = Storage::<Bytes32, FuelBlockDb>::get(self, &id)?.unwrap_or_default();
        Ok(block.headers.producer)
    }
}
