use alloy_rpc_types_trace::geth::{
    GethDebugBuiltInTracerType,
    GethDebugTracerType::{BuiltInTracer, JsTracer},
    GethDebugTracingCallOptions, GethDebugTracingOptions, GethTrace, NoopFrame,
    TraceResult,
};
use async_trait::async_trait;
use cfx_rpc_eth_api::DebugApiServer;
use cfx_rpc_eth_types::{BlockNumber, TransactionRequest};
use cfx_rpc_utils::error::invalid_params_msg;
use cfx_types::{Space, H256, U256};
use cfxcore::{ConsensusGraph, ConsensusGraphTrait, SharedConsensusGraph};
use geth_tracer::to_alloy_h256;
use jsonrpsee::core::RpcResult;
use primitives::{Block, BlockHeaderBuilder, EpochNumber};
use std::sync::Arc;

pub struct DebugApi {
    consensus: SharedConsensusGraph,
    max_estimation_gas_limit: Option<U256>,
}

impl DebugApi {
    pub fn new(
        consensus: SharedConsensusGraph, max_estimation_gas_limit: Option<U256>,
    ) -> Self {
        DebugApi {
            consensus,
            max_estimation_gas_limit,
        }
    }

    pub fn consensus_graph(&self) -> &ConsensusGraph {
        self.consensus
            .as_any()
            .downcast_ref::<ConsensusGraph>()
            .expect("downcast should succeed")
    }

    pub fn get_block_epoch_num(
        &self, block: BlockNumber,
    ) -> Result<u64, String> {
        let num = match block {
            BlockNumber::Num(block_number) => block_number,
            BlockNumber::Latest
            | BlockNumber::Safe
            | BlockNumber::Finalized => {
                let epoch_num = block.try_into().expect("should success");
                self.consensus_graph()
                    .get_height_from_epoch_number(epoch_num)?
            }
            BlockNumber::Hash {
                hash,
                require_canonical,
            } => self
                .consensus_graph()
                .get_block_epoch_number_with_pivot_check(
                    &hash,
                    require_canonical,
                )
                .map_err(|err| err.to_string())?,
            _ => return Err("not supported".to_string()),
        };
        Ok(num)
    }

    pub fn trace_call(
        &self, request: TransactionRequest, block_number: Option<BlockNumber>,
        opts: Option<GethDebugTracingCallOptions>,
    ) -> Result<GethTrace, String> {
        let opts = opts.unwrap_or_default();
        let block_num = block_number.unwrap_or_default();

        let epoch_num = self.get_block_epoch_num(block_num)?;

        // validate epoch state
        self.consensus_graph()
            .validate_stated_epoch(&EpochNumber::Number(epoch_num))?;

        let epoch_block_hashes = self
            .consensus_graph()
            .get_block_hashes_by_epoch(EpochNumber::Number(epoch_num))?;
        let epoch_id =
            epoch_block_hashes.last().expect("should have block hash");

        // construct blocks from call_request
        let chain_id = self.consensus.best_chain_id();
        // debug trace call has a fixed large gas limit.
        let signed_tx = request
            .sign_call(chain_id.in_evm_space(), self.max_estimation_gas_limit)
            .map_err(|err| err.to_string())?;
        let epoch_blocks = self
            .consensus_graph()
            .data_man
            .blocks_by_hash_list(
                &epoch_block_hashes,
                true, /* update_cache */
            )
            .expect("blocks exist");
        let pivot_block = epoch_blocks.last().expect("should have block");
        let header = BlockHeaderBuilder::new()
            .with_base_price(pivot_block.block_header.base_price())
            .with_parent_hash(pivot_block.block_header.hash())
            .with_height(epoch_num + 1)
            .with_timestamp(pivot_block.block_header.timestamp() + 1)
            .with_gas_limit(*pivot_block.block_header.gas_limit())
            .build();
        let block = Block::new(header, vec![Arc::new(signed_tx)]);
        let blocks: Vec<Arc<Block>> = vec![Arc::new(block)];

        let traces = self
            .consensus_graph()
            .collect_blocks_geth_trace(
                *epoch_id,
                epoch_num,
                &blocks,
                opts.tracing_options,
                None,
            )
            .map_err(|err| err.to_string())?;

        let res = traces.first().expect("should have trace");

        Ok(res.trace.clone())
    }

    pub fn trace_block_by_num(
        &self, block_num: u64, opts: Option<GethDebugTracingOptions>,
    ) -> Result<Vec<TraceResult>, String> {
        let opts = opts.unwrap_or_default();
        let epoch_traces = self
            .consensus_graph()
            .collect_epoch_geth_trace(block_num, None, opts)
            .map_err(|e| format!("invalid tx hash: {e}"))?;

        let result = epoch_traces
            .into_iter()
            .filter(|val| val.space == Space::Ethereum)
            .map(|val| TraceResult::Success {
                result: val.trace,
                tx_hash: Some(to_alloy_h256(val.tx_hash)),
            })
            .collect();
        Ok(result)
    }

    pub fn trace_transaction(
        &self, hash: H256, opts: Option<GethDebugTracingOptions>,
    ) -> Result<GethTrace, String> {
        let opts = opts.unwrap_or_default();

        // early return if tracer is not supported or NoopTracer is requested
        if let Some(tracer_type) = &opts.tracer {
            match tracer_type {
                BuiltInTracer(builtin_tracer) => match builtin_tracer {
                    GethDebugBuiltInTracerType::FourByteTracer => (),
                    GethDebugBuiltInTracerType::CallTracer => {
                        // pre check config
                        let _ = opts
                            .tracer_config
                            .clone()
                            .into_call_config()
                            .map_err(|err| err.to_string())?;
                        ()
                    }
                    GethDebugBuiltInTracerType::PreStateTracer => {
                        // pre check config
                        let _ = opts
                            .tracer_config
                            .clone()
                            .into_pre_state_config()
                            .map_err(|err| err.to_string())?;
                        ()
                    }
                    GethDebugBuiltInTracerType::NoopTracer => {
                        return Ok(GethTrace::NoopTracer(NoopFrame::default()))
                    }
                    GethDebugBuiltInTracerType::MuxTracer => {
                        return Err("not supported".into())
                    }
                },
                JsTracer(_) => return Err("not supported".into()),
            }
        }

        let tx_index = self
            .consensus
            .get_data_manager()
            .transaction_index_by_hash(&hash, false /* update_cache */)
            .ok_or("invalid tx hash")?;

        let epoch_num = self
            .consensus
            .get_block_epoch_number(&tx_index.block_hash)
            .ok_or("invalid tx hash")?;

        let epoch_traces = self
            .consensus_graph()
            .collect_epoch_geth_trace(epoch_num, Some(hash), opts)
            .map_err(|e| format!("invalid tx hash: {e}"))?;

        // filter by tx hash
        let trace = epoch_traces
            .into_iter()
            .find(|val| val.tx_hash == hash)
            .map(|val| val.trace)
            .ok_or("trace generation failed".to_string());

        trace
    }
}

#[async_trait]
impl DebugApiServer for DebugApi {
    async fn db_get(&self, _key: String) -> RpcResult<Option<String>> {
        Ok(Some("To be implemented!".into()))
    }

    async fn debug_trace_transaction(
        &self, tx_hash: H256, opts: Option<GethDebugTracingOptions>,
    ) -> RpcResult<GethTrace> {
        self.trace_transaction(tx_hash, opts)
            .map_err(|e| invalid_params_msg(&e))
    }

    async fn debug_trace_block_by_hash(
        &self, block_hash: H256, opts: Option<GethDebugTracingOptions>,
    ) -> RpcResult<Vec<TraceResult>> {
        let epoch_num = self
            .consensus_graph()
            .get_block_epoch_number_with_pivot_check(&block_hash, false)?;

        self.trace_block_by_num(epoch_num, opts)
            .map_err(|err| invalid_params_msg(&err))
    }

    async fn debug_trace_block_by_number(
        &self, block: BlockNumber, opts: Option<GethDebugTracingOptions>,
    ) -> RpcResult<Vec<TraceResult>> {
        let num = self
            .get_block_epoch_num(block)
            .map_err(|e| invalid_params_msg(&e))?;

        self.trace_block_by_num(num, opts)
            .map_err(|err| invalid_params_msg(&err))
    }

    async fn debug_trace_call(
        &self, request: TransactionRequest, block_number: Option<BlockNumber>,
        opts: Option<GethDebugTracingCallOptions>,
    ) -> RpcResult<GethTrace> {
        self.trace_call(request, block_number, opts)
            .map_err(|err| invalid_params_msg(&err))
    }
}
