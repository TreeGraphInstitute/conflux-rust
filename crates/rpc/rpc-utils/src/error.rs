use jsonrpc_core::Error as JsonRpcError;
use jsonrpsee::types::error::{ErrorObjectOwned, INVALID_PARAMS_CODE};

pub fn jsonrpc_error_to_error_object_owned(
    e: JsonRpcError,
) -> ErrorObjectOwned {
    ErrorObjectOwned::owned(e.code.code() as i32, e.message, e.data)
}

pub fn invalid_params_msg(param: &str) -> ErrorObjectOwned {
    invalid_params_rpc_err(format!("Invalid parameters: {}", param))
}

pub fn invalid_params_rpc_err(msg: impl Into<String>) -> ErrorObjectOwned {
    let data: Option<bool> = None;
    ErrorObjectOwned::owned(INVALID_PARAMS_CODE, msg.into(), data)
}
