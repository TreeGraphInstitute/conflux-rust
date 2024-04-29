#![allow(unused)]
mod arena;
mod builder;
mod config;
mod db_adapter;
mod gas;
mod tracing_inspector;
mod types;
mod utils;

pub use arena::CallTraceArena;
pub use builder::geth::{self, GethTraceBuilder};
use cfx_types::H160;
pub use config::{StackSnapshotType, TracingInspectorConfig};

use types::LogCallOrder;
use utils::{convert_h160, convert_h256, convert_u256};

use super::fourbyte::FourByteInspector;
use alloy_primitives::{Address, Bytes, LogData};
use revm::{
    db::InMemoryDB,
    interpreter::{Gas, InstructionResult, InterpreterResult},
    primitives::{ExecutionResult, ResultAndState, State},
};

use cfx_executor::{
    machine::Machine,
    observer::{
        CallTracer, CheckpointTracer, DrainTrace, InternalTransferTracer,
        OpcodeTracer, StorageTracer,
    },
    stack::{FrameResult, FrameReturn},
};

use cfx_vm_types::{ActionParams, CallType, Error, InterpreterInfo};

use alloy_rpc_types_trace::geth::{
    CallConfig, GethDebugBuiltInTracerType, GethDefaultTracingOptions,
    GethTrace, NoopFrame, PreStateConfig,
};
use tracing_inspector::TracingInspector;

use std::sync::Arc;

pub struct GethTracer {
    inner: TracingInspector,
    //
    tracer_type: Option<GethDebugBuiltInTracerType>,
    //
    fourbyte_inspector: FourByteInspector,
    //
    tx_gas_limit: u64,
    //
    gas_left: u64,
    // call depth
    depth: usize,
    //
    return_data: Bytes,
    //
    call_config: Option<CallConfig>,
    //
    prestate_config: Option<PreStateConfig>,
    //
    opcode_config: Option<GethDefaultTracingOptions>,
}

impl GethTracer {
    pub fn new(
        config: TracingInspectorConfig, tx_gas_limit: u64,
        machine: Arc<Machine>, tracer_type: Option<GethDebugBuiltInTracerType>,
        call_config: Option<CallConfig>,
        prestate_config: Option<PreStateConfig>,
        opcode_config: Option<GethDefaultTracingOptions>,
    ) -> Self {
        Self {
            inner: TracingInspector::new(config, machine),
            tracer_type,
            fourbyte_inspector: FourByteInspector::new(),
            tx_gas_limit,
            depth: 0,
            gas_left: tx_gas_limit,
            return_data: Bytes::default(),
            call_config,
            prestate_config,
            opcode_config,
        }
    }

    pub fn is_fourbyte_tracer(&self) -> bool {
        self.tracer_type == Some(GethDebugBuiltInTracerType::FourByteTracer)
    }

    pub fn gas_used(&self) -> u64 { self.tx_gas_limit - self.gas_left }

    pub fn drain(self) -> GethTrace {
        let trace = match self.tracer_type {
            Some(t) => match t {
                GethDebugBuiltInTracerType::FourByteTracer => {
                    self.fourbyte_inspector.drain()
                }
                GethDebugBuiltInTracerType::CallTracer => {
                    let gas_used = self.gas_used();
                    let opts = self.call_config.expect("should have config");
                    let frame = self
                        .inner
                        .into_geth_builder()
                        .geth_call_traces(opts, gas_used);
                    GethTrace::CallTracer(frame)
                }
                GethDebugBuiltInTracerType::PreStateTracer => {
                    // TODO replace the empty state and db with a real state
                    let gas_used = self.gas_used();
                    let opts =
                        self.prestate_config.expect("should have config");
                    let result = ResultAndState {
                        result: ExecutionResult::Revert {
                            gas_used,
                            output: Bytes::default(),
                        },
                        state: State::default(),
                    };
                    let db = InMemoryDB::default();
                    let frame = self
                        .inner
                        .into_geth_builder()
                        .geth_prestate_traces(&result, opts, db)
                        .unwrap();
                    GethTrace::PreStateTracer(frame)
                }
                GethDebugBuiltInTracerType::NoopTracer => {
                    GethTrace::NoopTracer(NoopFrame::default())
                }
                GethDebugBuiltInTracerType::MuxTracer => {
                    // not supported
                    GethTrace::NoopTracer(NoopFrame::default())
                }
            },
            None => {
                let gas_used = self.gas_used();
                let return_value = self.return_data;
                let opts = self.opcode_config.expect("should have config");
                let frame = self.inner.into_geth_builder().geth_traces(
                    gas_used,
                    return_value,
                    opts,
                );
                GethTrace::Default(frame)
            }
        };

        trace
    }
}

impl DrainTrace for GethTracer {
    fn drain_trace(self, map: &mut typemap::ShareDebugMap) {
        map.insert::<GethTraceKey>(self.drain());
    }
}

pub struct GethTraceKey;

impl typemap::Key for GethTraceKey {
    type Value = GethTrace;
}

impl CheckpointTracer for GethTracer {}

impl InternalTransferTracer for GethTracer {}

impl StorageTracer for GethTracer {}

impl CallTracer for GethTracer {
    fn record_call(&mut self, params: &ActionParams) {
        if self.is_fourbyte_tracer() {
            self.fourbyte_inspector.record_call(params);
            return;
        }

        self.depth += 1;
        self.inner.gas_stack.push(params.gas.clone());

        // determine correct `from` and `to` based on the call scheme
        let (from, to) = match params.call_type {
            CallType::DelegateCall | CallType::CallCode => {
                (params.address, params.code_address)
            }
            _ => (params.sender, params.address),
        };

        let value = if matches!(params.call_type, CallType::DelegateCall)
            && self.inner.active_trace().is_some()
        {
            // for delegate calls we need to use the value of the top trace
            let parent = self.inner.active_trace().unwrap();
            parent.trace.value
        } else {
            convert_u256(params.value.value())
        };

        // if calls to precompiles should be excluded, check whether this is a
        // call to a precompile
        let maybe_precompile =
            self.inner.config.exclude_precompile_calls.then(|| {
                self.inner.is_precompile_call(&to, value, params.space)
            });

        let to = convert_h160(to);
        let from = convert_h160(from);
        self.inner.start_trace_on_call(
            to,
            params.data.clone().unwrap_or_default().into(),
            value,
            params.call_type.into(),
            from,
            params.gas.as_u64(),
            maybe_precompile,
            self.tx_gas_limit,
            self.depth,
        );
    }

    fn record_call_result(&mut self, result: &FrameResult) {
        if self.is_fourbyte_tracer() {
            return;
        }

        self.depth -= 1;
        let mut gas_spent =
            self.inner.gas_stack.pop().expect("should have value");

        if let Ok(r) = result {
            gas_spent = gas_spent - r.gas_left;
            self.gas_left = r.gas_left.as_u64();
        }

        let instruction_result = to_instruction_result(result);

        if instruction_result.is_error() {
            self.inner.gas_inspector.set_gas_remainning(0);
        }

        let output = result
            .as_ref()
            .map(|f| Bytes::from(f.return_data.to_vec()))
            .unwrap_or_default();
        self.return_data = output.clone();

        let outcome = InterpreterResult {
            result: instruction_result,
            output,
            gas: Gas::default(),
        };

        self.inner
            .fill_trace_on_call_end(outcome, None, gas_spent.as_u64());
    }

    fn record_create(&mut self, params: &ActionParams) {
        if self.is_fourbyte_tracer() {
            return;
        }

        self.depth += 1;
        self.inner.gas_stack.push(params.gas.clone());

        let value = if matches!(params.call_type, CallType::DelegateCall) {
            // for delegate calls we need to use the value of the top trace
            if let Some(parent) = self.inner.active_trace() {
                parent.trace.value
            } else {
                convert_u256(params.value.value())
            }
        } else {
            convert_u256(params.value.value())
        };

        self.inner.start_trace_on_call(
            Address::default(), // call_result will set this address
            params.data.clone().unwrap_or_default().into(),
            value,
            params.call_type.into(),
            convert_h160(params.sender),
            params.gas.as_u64(),
            Some(false),
            params.gas.as_u64(),
            self.depth,
        );
    }

    fn record_create_result(&mut self, result: &FrameResult) {
        if self.is_fourbyte_tracer() {
            return;
        }

        self.depth -= 1;
        let mut gas_spent =
            self.inner.gas_stack.pop().expect("should have value");

        if let Ok(r) = result {
            gas_spent = gas_spent - r.gas_left;
            self.gas_left = r.gas_left.as_u64();
        }

        let instruction_result = to_instruction_result(result);

        if instruction_result.is_error() {
            self.inner.gas_inspector.set_gas_remainning(0);
        }

        let output = result
            .as_ref()
            .map(|f| Bytes::from(f.return_data.to_vec()))
            .unwrap_or_default();
        self.return_data = output.clone();

        let outcome = InterpreterResult {
            result: instruction_result,
            output,
            gas: Gas::default(),
        };

        let create_address =
            if let Ok(FrameReturn { create_address, .. }) = result {
                create_address.as_ref().map(|h| convert_h160(*h))
            } else {
                None
            };

        self.inner.fill_trace_on_call_end(
            outcome,
            create_address,
            gas_spent.as_u64(),
        );
    }
}

impl OpcodeTracer for GethTracer {
    fn do_trace_opcode(&self, enabled: &mut bool) {
        if self.inner.config.record_steps {
            *enabled |= true;
        }
    }

    fn initialize_interp(&mut self, gas_limit: cfx_types::U256) {
        self.inner
            .gas_inspector
            .set_gas_remainning(gas_limit.as_u64());
    }

    fn step(&mut self, interp: &dyn InterpreterInfo, depth: usize) {
        self.inner
            .gas_inspector
            .set_gas_remainning(interp.gas_remainning().as_u64());

        if self.inner.config.record_steps {
            self.inner.start_step(interp, depth as u64);
        }
    }

    fn step_end(&mut self, interp: &dyn InterpreterInfo) {
        let remainning = interp.gas_remainning().as_u64();
        let last_gas_cost = self
            .inner
            .gas_inspector
            .gas_remaining()
            .saturating_sub(remainning);
        self.inner.gas_inspector.set_gas_remainning(remainning);
        self.inner.gas_inspector.set_last_gas_cost(last_gas_cost);

        // trace
        if self.inner.config.record_steps {
            self.inner.fill_step_on_step_end(interp);
        }
    }

    fn log(
        &mut self, _address: &cfx_types::Address, topics: Vec<cfx_types::H256>,
        data: &[u8],
    ) {
        if self.inner.config.record_logs {
            let trace_idx = self.inner.last_trace_idx();
            let trace = &mut self.inner.traces.arena[trace_idx];
            trace.ordering.push(LogCallOrder::Log(trace.logs.len()));
            trace.logs.push(LogData::new_unchecked(
                topics.iter().map(|f| convert_h256(*f)).collect(),
                Bytes::from(data.to_vec()),
            ));
        }
    }

    fn selfdestruct(
        &mut self, _contract: &cfx_types::Address, target: &cfx_types::Address,
        _value: cfx_types::U256,
    ) {
        if self.is_fourbyte_tracer() {
            return;
        }

        let trace_idx = self.inner.last_trace_idx();
        let trace = &mut self.inner.traces.arena[trace_idx].trace;
        trace.selfdestruct_refund_target = Some(convert_h160(*target as H160))
    }
}

#[derive(Clone, Copy, Debug)]
struct StackStep {
    trace_idx: usize,
    step_idx: usize,
}

pub fn to_instruction_result(frame_result: &FrameResult) -> InstructionResult {
    let result = match frame_result {
        Ok(_r) => InstructionResult::Return, // todo check this
        Err(err) => match err {
            Error::OutOfGas => InstructionResult::OutOfGas,
            Error::BadJumpDestination { destination: _ } => {
                InstructionResult::InvalidJump
            }
            Error::BadInstruction { instruction: _ } => {
                InstructionResult::OpcodeNotFound
            }
            Error::StackUnderflow {
                instruction: _,
                wanted: _,
                on_stack: _,
            } => InstructionResult::StackUnderflow,
            Error::OutOfStack { .. } => InstructionResult::StackOverflow,
            Error::SubStackUnderflow { .. } => {
                InstructionResult::StackUnderflow
            }
            Error::OutOfSubStack {
                wanted: _,
                limit: _,
            } => InstructionResult::StackOverflow,
            Error::InvalidSubEntry => InstructionResult::NotActivated, //
            Error::NotEnoughBalanceForStorage {
                required: _,
                got: _,
            } => InstructionResult::OutOfFunds,
            Error::ExceedStorageLimit => InstructionResult::OutOfGas, /* treat storage as gas */
            Error::BuiltIn(_) => InstructionResult::PrecompileError,
            Error::InternalContract(_) => InstructionResult::PrecompileError, /* treat internalContract as builtIn */
            Error::MutableCallInStaticContext => {
                InstructionResult::StateChangeDuringStaticCall
            }
            Error::StateDbError(_) => InstructionResult::FatalExternalError,
            Error::Wasm(_) => InstructionResult::NotActivated,
            Error::OutOfBounds => InstructionResult::OutOfOffset,
            Error::Reverted => InstructionResult::Revert,
            Error::InvalidAddress(_) => todo!(), /* when selfdestruct refund */
            // address is invalid will
            // emit this error
            Error::ConflictAddress(_) => InstructionResult::CreateCollision,
        },
    };
    result
}
