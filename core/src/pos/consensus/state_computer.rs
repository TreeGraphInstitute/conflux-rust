// Copyright (c) The Diem Core Contributors
// SPDX-License-Identifier: Apache-2.0

use super::{error::StateSyncError, state_replication::StateComputer};
use anyhow::Result;
use consensus_types::block::Block;
use diem_crypto::HashValue;
use diem_infallible::Mutex;
use diem_logger::prelude::*;
use diem_metrics::monitor;
use diem_types::ledger_info::LedgerInfoWithSignatures;
use executor_types::{
    BlockExecutor, Error as ExecutionError, StateComputeResult,
};
use fail::fail_point;
//use state_sync::client::StateSyncClient;
use diem_types::transaction::Transaction;
use state_sync::client::StateSyncClient;
use std::boxed::Box;

/// Basic communication with the Execution module;
/// implements StateComputer traits.
pub struct ExecutionProxy {
    //execution_correctness_client:
    //    Mutex<Box<dyn ExecutionCorrectness + Send + Sync>>,
    synchronizer: StateSyncClient,
    // TODO(lpl): Use Mutex or Arc?
    executor: Mutex<Box<dyn BlockExecutor>>,
}

impl ExecutionProxy {
    pub fn new(
        executor: Box<dyn BlockExecutor>, synchronizer: StateSyncClient,
    ) -> Self {
        Self {
            /*execution_correctness_client: Mutex::new(
                execution_correctness_client,
            ),*/
            synchronizer,
            executor: Mutex::new(executor),
        }
    }
}

#[async_trait::async_trait]
impl StateComputer for ExecutionProxy {
    fn compute(
        &self,
        // The block to be executed.
        block: &Block,
        // The parent block id.
        parent_block_id: HashValue,
    ) -> Result<StateComputeResult, ExecutionError>
    {
        fail_point!("consensus::compute", |_| {
            Err(ExecutionError::InternalError {
                error: "Injected error in compute".into(),
            })
        });
        diem_debug!(
            block_id = block.id(),
            parent_id = block.parent_id(),
            "Executing block",
        );

        // TODO: figure out error handling for the prologue txn
        monitor!(
            "execute_block",
            self.executor.lock().execute_block(
                id_and_transactions_from_block(block),
                parent_block_id
            )
        )
    }

    /// Send a successful commit. A future is fulfilled when the state is
    /// finalized.
    async fn commit(
        &self, block_ids: Vec<HashValue>,
        finality_proof: LedgerInfoWithSignatures,
    ) -> Result<(), ExecutionError>
    {
        let (committed_txns, reconfig_events) = monitor!(
            "commit_block",
            self.executor
                .lock()
                .commit_blocks(block_ids, finality_proof)?
        );
        if let Err(e) = monitor!(
            "notify_state_sync",
            self.synchronizer
                .commit(committed_txns, reconfig_events)
                .await
        ) {
            diem_error!(error = ?e, "Failed to notify state synchronizer");
        }
        Ok(())
    }

    /// Synchronize to a commit that not present locally.
    async fn sync_to(
        &self, target: LedgerInfoWithSignatures,
    ) -> Result<(), StateSyncError> {
        fail_point!("consensus::sync_to", |_| {
            Err(anyhow::anyhow!("Injected error in sync_to").into())
        });
        // Here to start to do state synchronization where ChunkExecutor inside
        // will process chunks and commit to Storage. However, after
        // block execution and commitments, the the sync state of
        // ChunkExecutor may be not up to date so it is required to
        // reset the cache of ChunkExecutor in State Sync when requested
        // to sync.
        //let res = monitor!("sync_to",
        // self.synchronizer.sync_to(target).await); Similarily, after
        // the state synchronization, we have to reset the
        // cache of BlockExecutor to guarantee the latest committed
        // state is up to date.
        //self.executor.reset()?;

        /*res.map_err(|error| {
            let anyhow_error: anyhow::Error = error.into();
            anyhow_error.into()
        })*/
        Ok(())
    }
}

fn id_and_transactions_from_block(
    block: &Block,
) -> (HashValue, Vec<Transaction>) {
    let id = block.id();
    // TODO(lpl): Do we need BlockMetadata?
    let mut transactions = vec![Transaction::BlockMetadata(block.into())];
    transactions.extend(
        block
            .payload()
            .unwrap_or(&vec![])
            .iter()
            .map(|txn| Transaction::UserTransaction(txn.clone())),
    );
    (id, transactions)
}
