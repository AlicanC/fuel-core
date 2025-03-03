use crate::{types::*, Error};
use fuel_core_interfaces::txpool::TxPoolDb;
use fuel_tx::{Input, Output, UtxoId};
use std::collections::{HashMap, HashSet};

/// Check and hold dependency between inputs and outputs. Be mindful
/// about depth of connection
#[derive(Debug, Clone)]
pub struct Dependency {
    /// maping of all UtxoId relationships in txpool
    coins: HashMap<UtxoId, CoinState>,
    /// Contract-> Tx mapping.
    contracts: HashMap<ContractId, ContractState>,
    /// max depth of dependency.
    max_depth: usize,
}

#[derive(Debug, Clone)]
pub struct CoinState {
    /// is Utxo spend as other Tx input
    is_spend_by: Option<TxId>,
    /// how deep are we inside UTXO dependency
    depth: usize,
}

#[derive(Debug, Clone)]
pub struct ContractState {
    /// is Utxo spend as other Tx input
    used_by: HashSet<TxId>,
    /// how deep are we inside UTXO dependency
    depth: usize,
    /// origin is needed for child to parent rel, in case when contract is in dependency this is how we make a chain.
    origin: Option<UtxoId>,
    /// gas_price. We can probably derive this from Tx
    gas_price: GasPrice,
}

impl Dependency {
    pub fn new(max_depth: usize) -> Self {
        Self {
            coins: HashMap::new(),
            contracts: HashMap::new(),
            max_depth,
        }
    }

    /// find all dependent Transactions that are inside txpool.
    /// Does not check db. They can be sorted by gasPrice to get order of dependency
    pub fn find_dependent(
        &self,
        tx: ArcTx,
        seen: &mut HashMap<TxId, ArcTx>,
        txs: &HashMap<TxId, ArcTx>,
    ) {
        // for every input aggregate UtxoId and check if it is inside
        let mut check = vec![tx.id()];
        while let Some(parent_txhash) = check.pop() {
            let mut is_new = false;
            let mut parent_tx = None;
            seen.entry(parent_txhash).or_insert_with(|| {
                is_new = true;
                let parent = txs.get(&parent_txhash).expect("To have tx in txpool");
                parent_tx = Some(parent.clone());
                parent.clone()
            });
            // for every input check if tx_id is inside seen. if not, check coins/contract map.
            if let Some(parent_tx) = parent_tx {
                for input in parent_tx.inputs() {
                    // if found and depth is not zero add it to `check`.
                    match input {
                        Input::Coin { utxo_id, .. } => {
                            let state = self
                                .coins
                                .get(utxo_id)
                                .expect("to find coin inside spend tx");
                            // if depth is not zero it means tx is inside txpool. Zero == db utxo
                            if state.depth != 0 {
                                check.push(*utxo_id.tx_id());
                            }
                        }
                        Input::Contract { contract_id, .. } => {
                            let state = self
                                .contracts
                                .get(contract_id)
                                .expect("Expect to find contract in dependency");

                            if state.depth != 0 {
                                let origin = state
                                    .origin
                                    .as_ref()
                                    .expect("contract origin to be present");
                                check.push(*origin.tx_id());
                            }
                        }
                    }
                }
            }
        }
    }

    fn check_if_coin_input_can_spend_output(
        output: &Output,
        input: &Input,
        is_output_filled: bool,
    ) -> anyhow::Result<()> {
        if let Input::Coin {
            owner,
            amount,
            asset_id,
            ..
        } = input
        {
            let i_owner = owner;
            let i_amount = amount;
            let i_asset_id = asset_id;
            match output {
                Output::Coin {
                    to,
                    amount,
                    asset_id,
                } => {
                    if to != i_owner {
                        return Err(Error::NotInsertedIoWrongOwner.into());
                    }
                    if amount != i_amount {
                        return Err(Error::NotInsertedIoWrongAmount.into());
                    }
                    if asset_id != i_asset_id {
                        return Err(Error::NotInsertedIoWrongAssetId.into());
                    }
                }
                Output::Contract { .. } => return Err(Error::NotInsertedIoConractOutput.into()),
                Output::Withdrawal { .. } => {
                    return Err(Error::NotInsertedIoWithdrawalInput.into());
                }
                Output::Change {
                    to,
                    asset_id,
                    amount,
                } => {
                    if to != i_owner {
                        return Err(Error::NotInsertedIoWrongOwner.into());
                    }
                    if asset_id != i_asset_id {
                        return Err(Error::NotInsertedIoWrongAssetId.into());
                    }
                    if is_output_filled && amount != i_amount {
                        return Err(Error::NotInsertedIoWrongAmount.into());
                    }
                }
                Output::Variable {
                    to,
                    amount,
                    asset_id,
                } => {
                    if is_output_filled {
                        if to != i_owner {
                            return Err(Error::NotInsertedIoWrongOwner.into());
                        }
                        if amount != i_amount {
                            return Err(Error::NotInsertedIoWrongAmount.into());
                        }
                        if asset_id != i_asset_id {
                            return Err(Error::NotInsertedIoWrongAssetId.into());
                        }
                    }
                    // else do nothing, everything is variable and can be only check on execution
                }
                Output::ContractCreated { .. } => {
                    return Err(Error::NotInsertedIoConractOutput.into())
                }
            };
        } else {
            panic!("Use it only for coin output check");
        }
        Ok(())
    }

    /// Check for colision. Used only inside insert function.
    /// Id doesn't change any dependency it just checks if it has posibility to be included.
    /// Returns: (max_depth, db_coins, db_contracts, collided_transactions);
    #[allow(clippy::type_complexity)]
    fn check_for_colision<'a>(
        &'a self,
        txs: &'a HashMap<TxId, ArcTx>,
        db: &dyn TxPoolDb,
        tx: &'a ArcTx,
    ) -> anyhow::Result<(
        usize,
        HashMap<UtxoId, CoinState>,
        HashMap<ContractId, ContractState>,
        Vec<TxId>,
    )> {
        let mut collided: Vec<TxId> = Vec::new();
        // iterate over all inputs and check for colision
        let mut max_depth = 0;
        let mut db_coins: HashMap<UtxoId, CoinState> = HashMap::new();
        let mut db_contracts: HashMap<ContractId, ContractState> = HashMap::new();
        for input in tx.inputs() {
            // check if all required inputs are here.
            match input {
                Input::Coin { utxo_id, .. } => {
                    // is it dependent output?
                    if let Some(state) = self.coins.get(utxo_id) {
                        // check depth
                        max_depth = core::cmp::max(state.depth + 1, max_depth);
                        if max_depth > self.max_depth {
                            return Err(Error::NotInsertedMaxDepth.into());
                        }
                        // output is present but is it spend by other tx?
                        if let Some(ref spend_by) = state.is_spend_by {
                            // get tx that is spending this output
                            let txpool_tx = txs
                                .get(spend_by)
                                .expect("Tx should be always present in txpool");
                            // compare if tx has better price
                            if txpool_tx.gas_price() > tx.gas_price() {
                                return Err(Error::NotInsertedCollision(*spend_by, *utxo_id).into());
                            } else {
                                if state.depth == 0 {
                                    //this means it is loaded from db. Get tx to compare output.
                                    let db_tx = db.transaction(*utxo_id.tx_id())?;
                                    let output = if let Some(ref db_tx) = db_tx {
                                        if let Some(output) =
                                            db_tx.outputs().get(utxo_id.output_index() as usize)
                                        {
                                            output
                                        } else {
                                            // output out of bound
                                            return Err(Error::NotInsertedOutputNotExisting(
                                                *utxo_id,
                                            )
                                            .into());
                                        }
                                    } else {
                                        return Err(Error::NotInsertedInputTxNotExisting(
                                            *utxo_id.tx_id(),
                                        )
                                        .into());
                                    };
                                    Self::check_if_coin_input_can_spend_output(
                                        output, input, true,
                                    )?;
                                } else {
                                    // tx output is in pool
                                    let output_tx = txs.get(utxo_id.tx_id()).unwrap();
                                    let output =
                                        &output_tx.outputs()[utxo_id.output_index() as usize];
                                    Self::check_if_coin_input_can_spend_output(
                                        output, input, false,
                                    )?;
                                };

                                collided.push(*spend_by);
                            }
                        }
                        // if coin is not spend, it will be spend later down the line
                    } else {
                        // fetch from db and check if tx exist.
                        if let Some(db_tx) = db.transaction(*utxo_id.tx_id())? {
                            if let Some(output) =
                                db_tx.outputs().get(utxo_id.output_index() as usize)
                            {
                                // add depth
                                max_depth = core::cmp::max(1, max_depth);
                                // do all checks that we can do
                                Self::check_if_coin_input_can_spend_output(output, input, true)?;
                                // insert it as spend coin.
                                // check for double spend should be done before transaction is received.
                                db_coins.insert(
                                    *utxo_id,
                                    CoinState {
                                        is_spend_by: Some(tx.id() as TxId),
                                        depth: 0,
                                    },
                                );
                            } else {
                                // output out of bound
                                return Err(Error::NotInsertedOutputNotExisting(*utxo_id).into());
                            }
                        } else {
                            return Err(
                                Error::NotInsertedInputTxNotExisting(*utxo_id.tx_id()).into()
                            );
                        }
                    }

                    // yey we got our coin
                }
                Input::Contract { contract_id, .. } => {
                    // Does contract exist. We dont need to do any check here other then if contract_id exist or not.
                    if let Some(state) = self.contracts.get(contract_id) {
                        // check if contract is created after this transaction.
                        if tx.gas_price() > state.gas_price {
                            return Err(Error::NotInsertedContractPricedLower(*contract_id).into());
                        }
                        // check depth.
                        max_depth = core::cmp::max(state.depth, max_depth);
                        if max_depth > self.max_depth {
                            return Err(Error::NotInsertedMaxDepth.into());
                        }
                    } else {
                        if !db.contract_exist(*contract_id)? {
                            return Err(
                                Error::NotInsertedInputContractNotExisting(*contract_id).into()
                            );
                        }
                        // add depth
                        max_depth = core::cmp::max(1, max_depth);
                        // insert into contract
                        db_contracts
                            .entry(*contract_id)
                            .or_insert(ContractState {
                                used_by: HashSet::new(),
                                depth: 0,
                                origin: None, //there is no owner if contract is in db
                                gas_price: GasPrice::MAX,
                            })
                            .used_by
                            .insert(tx.id());
                    }

                    // yey we got our contract
                }
            }
        }

        // nice, our inputs don't collide. Now check if our newly created contracts collide on ContractId
        for output in tx.outputs() {
            if let Output::ContractCreated { contract_id, .. } = output {
                if let Some(contract) = self.contracts.get(contract_id) {
                    // we have a collision :(
                    if contract.depth == 0 {
                        // if depth is zero it means it is contract from db
                        return Err(Error::NotInsertedContractIdAlreadyTaken(*contract_id).into());
                    }
                    // check who is priced more
                    if contract.gas_price > tx.gas_price() {
                        // new tx is priced less then current tx
                        return Err(Error::NotInsertedCollisionContractId(*contract_id).into());
                    }
                    // if we are prices more, mark current contract origin for removal.
                    let origin = contract.origin.expect(
                            "Only contract without origin are the ones that are inside DB. And we check depth for that, so we are okay to just unwrap
                        ");
                    collided.push(*origin.tx_id());
                }
            }
            // collision of other outputs is not possible.
        }

        Ok((max_depth, db_coins, db_contracts, collided))
    }
    /// insert tx inside dependency
    /// return list of transactions that are removed from txpool
    pub async fn insert<'a>(
        &'a mut self,
        txs: &'a HashMap<TxId, ArcTx>,
        db: &dyn TxPoolDb,
        tx: &'a ArcTx,
    ) -> anyhow::Result<Vec<ArcTx>> {
        let (max_depth, db_coins, db_contracts, collided) = self.check_for_colision(txs, db, tx)?;

        // now we are sure that transaction can be included. remove all collided transactions
        let mut removed_tx = Vec::new();
        for collided in collided.into_iter() {
            let collided = txs
                .get(&collided)
                .expect("Collided should be present in txpool");
            removed_tx.extend(self.recursively_remove_all_dependencies(txs, collided.clone()));
        }

        // iterate over all inputs and spend parent coins/contracts
        for input in tx.inputs() {
            match input {
                Input::Coin { utxo_id, .. } => {
                    // spend coin
                    if let Some(state) = self.coins.get_mut(utxo_id) {
                        state.is_spend_by = Some(tx.id());
                    }
                }
                Input::Contract { contract_id, .. } => {
                    // Contract that we want to use can be already inside dependency (this case)
                    // or it will be added when db_contracts extends self.contracts (and it
                    // already contains changed used_by)
                    if let Some(state) = self.contracts.get_mut(contract_id) {
                        state.used_by.insert(tx.id());
                    }
                }
            }
        }

        // insert all coins/contracts that we got from db;
        self.coins.extend(db_coins.into_iter());
        // for contracts from db that are not found in dependency, we already inserted used_by
        // and are okay to just extend current list
        self.contracts.extend(db_contracts.into_iter());

        // iterate over all outputs and insert them, marking them as available.
        for (index, output) in tx.outputs().iter().enumerate() {
            let utxo_id = UtxoId::new(tx.id(), index as u8);
            match output {
                Output::Coin { .. } | Output::Change { .. } | Output::Variable { .. } => {
                    // insert output coin inside by_coin
                    self.coins.insert(
                        utxo_id,
                        CoinState {
                            is_spend_by: None,
                            depth: max_depth,
                        },
                    );
                }
                Output::ContractCreated { contract_id, .. } => {
                    // insert contract
                    self.contracts.insert(
                        *contract_id,
                        ContractState {
                            depth: max_depth,
                            used_by: HashSet::new(),
                            origin: Some(utxo_id),
                            gas_price: tx.gas_price(),
                        },
                    );
                }
                Output::Withdrawal { .. } => {
                    // withdrawal does nothing and it should not be found in dependency.
                }
                Output::Contract { .. } => {
                    // do nothing, this contract is already already found in dependencies.
                    // as it is tied with input and used_by is already inserted.
                }
            };
        }

        Ok(removed_tx)
    }

    pub fn recursively_remove_all_dependencies<'a>(
        &'a mut self,
        txs: &'a HashMap<TxId, ArcTx>,
        tx: ArcTx,
    ) -> Vec<ArcTx> {
        let mut removed_tx = vec![tx.clone()];

        // for all outputs recursivly call removal
        for (index, output) in tx.outputs().iter().enumerate() {
            // get contract_id or none if it is coin
            let is_contract = match output {
                Output::Coin { .. }
                | Output::Withdrawal { .. }
                | Output::Change { .. }
                | Output::Variable { .. } => None,
                Output::Contract { input_index, .. } => {
                    if let Input::Contract { contract_id, .. } = tx.inputs()[*input_index as usize]
                    {
                        Some(contract_id)
                    } else {
                        panic!("InputIndex for contract should be always correct");
                    }
                }
                Output::ContractCreated { contract_id, .. } => Some(*contract_id),
            };
            // remove contract childrens.
            if let Some(ref contract_id) = is_contract {
                let state = self.contracts.get(contract_id).cloned();
                if let Some(state) = state {
                    // call removal on every tx;
                    for rem_tx in state.used_by.iter() {
                        let rem_tx = txs.get(rem_tx).expect("Tx should be present in txs");
                        removed_tx
                            .extend(self.recursively_remove_all_dependencies(txs, rem_tx.clone()));
                    }
                } else {
                    panic!("Contract state should be present when removing child");
                }
                // remove it from dependency
                self.contracts.remove(contract_id);
            } else {
                // remove user of this coin.
                let utxo = UtxoId::new(tx.id(), index as u8);
                // call childrens.
                if let Some(spend_by) = self
                    .coins
                    .get(&utxo)
                    .expect("Coin should be present when removing child")
                    .is_spend_by
                {
                    let rem_tx = txs.get(&spend_by).expect("Tx should be present in txs");
                    removed_tx
                        .extend(self.recursively_remove_all_dependencies(txs, rem_tx.clone()));
                }
                // remove it from dependency
                self.coins.remove(&utxo);
            }
        }

        // set all inputs as unspend.
        for input in tx.inputs() {
            match input {
                Input::Coin { utxo_id, .. } => {
                    let mut rem_coin = false;
                    {
                        let state = self.coins.get_mut(utxo_id).expect("Coin should be present");
                        state.is_spend_by = None;
                        if state.depth == 0 {
                            rem_coin = true;
                        }
                    }
                    if rem_coin {
                        self.coins.remove(utxo_id);
                    }
                }
                Input::Contract { contract_id, .. } => {
                    // remove from contract
                    let mut rem_contract = false;
                    {
                        let state = self
                            .contracts
                            .get_mut(contract_id)
                            .expect("Contract should be present");
                        state.used_by.remove(&tx.id());
                        // if contract list is empty, remove contract from dependency.
                        if state.used_by.is_empty() {
                            rem_contract = true;
                        }
                    }
                    if rem_contract {
                        self.contracts.remove(contract_id);
                    }
                }
            }
        }

        removed_tx
    }
}

#[cfg(test)]
mod tests {

    use std::str::FromStr;

    use fuel_tx::{Address, AssetId, UtxoId};
    use fuel_types::Bytes32;

    use super::*;

    #[test]
    fn test_check_between_input_output() {
        let output = Output::Coin {
            to: Address::default(),
            amount: 10,
            asset_id: AssetId::default(),
        };
        let input = Input::Coin {
            utxo_id: UtxoId::default(),
            owner: Address::default(),
            amount: 10,
            asset_id: AssetId::default(),
            witness_index: 0,
            maturity: 0,
            predicate: Vec::new(),
            predicate_data: Vec::new(),
        };
        let out = Dependency::check_if_coin_input_can_spend_output(&output, &input, false);
        assert!(out.is_ok(), "test1. It should be Ok");

        let output = Output::Coin {
            to: Address::default(),
            amount: 12,
            asset_id: AssetId::default(),
        };

        let out = Dependency::check_if_coin_input_can_spend_output(&output, &input, false);
        assert!(out.is_err(), "test2 There should be error");
        assert_eq!(
            out.err().unwrap().downcast_ref(),
            Some(&Error::NotInsertedIoWrongAmount),
            "test2"
        );

        let output = Output::Coin {
            to: Address::from_str(
                "0x0000000000000000000000000000000000000000000000000000000000000001",
            )
            .unwrap(),
            amount: 10,
            asset_id: AssetId::default(),
        };

        let out = Dependency::check_if_coin_input_can_spend_output(&output, &input, false);
        assert!(out.is_err(), "test3 There should be error");
        assert_eq!(
            out.err().unwrap().downcast_ref(),
            Some(&Error::NotInsertedIoWrongOwner),
            "test3"
        );

        let output = Output::Coin {
            to: Address::default(),
            amount: 10,
            asset_id: AssetId::from_str(
                "0x0000000000000000000000000000000000000000000000000000000000000001",
            )
            .unwrap(),
        };

        let out = Dependency::check_if_coin_input_can_spend_output(&output, &input, false);
        assert!(out.is_err(), "test4 There should be error");
        assert_eq!(
            out.err().unwrap().downcast_ref(),
            Some(&Error::NotInsertedIoWrongAssetId),
            "test4"
        );

        let output = Output::Coin {
            to: Address::default(),
            amount: 10,
            asset_id: AssetId::from_str(
                "0x0000000000000000000000000000000000000000000000000000000000000001",
            )
            .unwrap(),
        };

        let out = Dependency::check_if_coin_input_can_spend_output(&output, &input, false);
        assert!(out.is_err(), "test5 There should be error");
        assert_eq!(
            out.err().unwrap().downcast_ref(),
            Some(&Error::NotInsertedIoWrongAssetId),
            "test5"
        );

        let output = Output::Contract {
            input_index: 0,
            balance_root: Default::default(),
            state_root: Default::default(),
        };

        let out = Dependency::check_if_coin_input_can_spend_output(&output, &input, false);
        assert!(out.is_err(), "test6 There should be error");
        assert_eq!(
            out.err().unwrap().downcast_ref(),
            Some(&Error::NotInsertedIoConractOutput),
            "test6"
        );

        let output = Output::Withdrawal {
            to: Default::default(),
            amount: Default::default(),
            asset_id: Default::default(),
        };

        let out = Dependency::check_if_coin_input_can_spend_output(&output, &input, false);
        assert!(out.is_err(), "test7 There should be error");
        assert_eq!(
            out.err().unwrap().downcast_ref(),
            Some(&Error::NotInsertedIoWithdrawalInput),
            "test7"
        );

        let output = Output::ContractCreated {
            contract_id: ContractId::default(),
            state_root: Bytes32::default(),
        };

        let out = Dependency::check_if_coin_input_can_spend_output(&output, &input, false);
        assert!(out.is_err(), "test8 There should be error");
        assert_eq!(
            out.err().unwrap().downcast_ref(),
            Some(&Error::NotInsertedIoConractOutput),
            "test8"
        );
    }
}
