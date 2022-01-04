// Copyright 2019 Conflux Foundation. All rights reserved.
// Conflux is free software and distributed under GNU General Public License.
// See http://www.gnu.org/licenses/

use crate::{bytes::Bytes, hash::keccak};
use cfx_types::{
    Address, AddressSpaceUtil, AddressWithSpace, BigEndianHash, Space, H160,
    H256, U256,
};
use keylib::{
    self, public_to_address, recover, verify_public, Public, Secret, Signature,
};
use malloc_size_of::{MallocSizeOf, MallocSizeOfOps};
use rlp::{self, Decodable, DecoderError, Encodable, Rlp, RlpStream};
use rlp_derive::{RlpDecodable, RlpEncodable};
use serde::{Deserialize, Serialize};
use std::{error, fmt, ops::Deref};
use unexpected::OutOfBounds;

/// Fake address for unsigned transactions.
pub const UNSIGNED_SENDER: Address = H160([0xff; 20]);

/// Shorter id for transactions in compact blocks
// TODO should be u48
pub type TxShortId = u64;

pub type TxPropagateId = u32;

// FIXME: Most errors here are bounded for TransactionPool and intended for rpc,
// FIXME: however these are unused, they are not errors for transaction itself.
// FIXME: Transaction verification and consensus related error can be separated.
#[derive(Debug, PartialEq, Clone)]
/// Errors concerning transaction processing.
pub enum TransactionError {
    /// Transaction is already imported to the queue
    AlreadyImported,
    /// Chain id in the transaction doesn't match the chain id of the network.
    ChainIdMismatch {
        expected: u32,
        got: u32,
        space: Space,
    },
    /// Epoch height out of bound.
    EpochHeightOutOfBound {
        block_height: u64,
        set: u64,
        transaction_epoch_bound: u64,
    },
    /// The gas paid for transaction is lower than base gas.
    NotEnoughBaseGas {
        /// Absolute minimum gas required.
        required: U256,
        /// Gas provided.
        got: U256,
    },
    /// Transaction is not valid anymore (state already has higher nonce)
    Stale,
    /// Transaction has too low fee
    /// (there is already a transaction with the same sender-nonce but higher
    /// gas price)
    TooCheapToReplace,
    /// Transaction was not imported to the queue because limit has been
    /// reached.
    LimitReached,
    /// Transaction's gas price is below threshold.
    InsufficientGasPrice {
        /// Minimal expected gas price
        minimal: U256,
        /// Transaction gas price
        got: U256,
    },
    /// Transaction's gas is below currently set minimal gas requirement.
    InsufficientGas {
        /// Minimal expected gas
        minimal: U256,
        /// Transaction gas
        got: U256,
    },
    /// Sender doesn't have enough funds to pay for this transaction
    InsufficientBalance {
        /// Senders balance
        balance: U256,
        /// Transaction cost
        cost: U256,
    },
    /// Transactions gas is higher then current gas limit
    GasLimitExceeded {
        /// Current gas limit
        limit: U256,
        /// Declared transaction gas
        got: U256,
    },
    /// Transaction's gas limit (aka gas) is invalid.
    InvalidGasLimit(OutOfBounds<U256>),
    /// Signature error
    InvalidSignature(String),
    /// Transaction too big
    TooBig,
    /// Invalid RLP encoding
    InvalidRlp(String),
    ZeroGasPrice,
    /// Ethereum-like transaction with invalid storage limit.
    InvalidEthereumLike,
    /// Receiver with invalid type bit.
    InvalidReceiver,
}

impl From<keylib::Error> for TransactionError {
    fn from(err: keylib::Error) -> Self {
        TransactionError::InvalidSignature(format!("{}", err))
    }
}

impl From<rlp::DecoderError> for TransactionError {
    fn from(err: rlp::DecoderError) -> Self {
        TransactionError::InvalidRlp(format!("{}", err))
    }
}

impl fmt::Display for TransactionError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        use self::TransactionError::*;
        let msg = match *self {
            AlreadyImported => "Already imported".into(),
            ChainIdMismatch { expected, got, space } => {
                format!("Chain id mismatch, expected {}, got {}, space {:?}", expected, got,space)
            }
            EpochHeightOutOfBound {
                block_height,
                set,
                transaction_epoch_bound,
            } => format!(
                "EpochHeight out of bound:\
                 block_height {}, transaction epoch_height {}, transaction_epoch_bound {}",
                block_height, set, transaction_epoch_bound
            ),
            NotEnoughBaseGas { got, required } => format!(
                "Transaction gas {} less than intrinsic gas {}",
                got, required
            ),
            Stale => "No longer valid".into(),
            TooCheapToReplace => "Gas price too low to replace".into(),
            LimitReached => "Transaction limit reached".into(),
            InsufficientGasPrice { minimal, got } => format!(
                "Insufficient gas price. Min={}, Given={}",
                minimal, got
            ),
            InsufficientGas { minimal, got } => {
                format!("Insufficient gas. Min={}, Given={}", minimal, got)
            }
            InsufficientBalance { balance, cost } => format!(
                "Insufficient balance for transaction. Balance={}, Cost={}",
                balance, cost
            ),
            GasLimitExceeded { limit, got } => {
                format!("Gas limit exceeded. Limit={}, Given={}", limit, got)
            }
            InvalidGasLimit(ref err) => format!("Invalid gas limit. {}", err),
            InvalidSignature(ref err) => {
                format!("Transaction has invalid signature: {}.", err)
            }
            TooBig => "Transaction too big".into(),
            InvalidRlp(ref err) => {
                format!("Transaction has invalid RLP structure: {}.", err)
            }
            ZeroGasPrice => "Zero gas price is not allowed".into(),
            InvalidEthereumLike => "Ethereum like transaction should have u64::MAX storage limit".into(),
            InvalidReceiver => "Sending transaction to invalid address. The first four bits of address must be 0x0, 0x1, or 0x8.".into(),
        };

        f.write_fmt(format_args!("Transaction error ({})", msg))
    }
}

impl error::Error for TransactionError {
    fn description(&self) -> &str { "Transaction error" }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Action {
    /// Create creates new contract.
    Create,
    /// Calls contract at given address.
    /// In the case of a transfer, this is the receiver's address.'
    Call(Address),
}

impl Default for Action {
    fn default() -> Action { Action::Create }
}

impl Decodable for Action {
    fn decode(rlp: &Rlp) -> Result<Self, DecoderError> {
        if rlp.is_empty() {
            Ok(Action::Create)
        } else {
            Ok(Action::Call(rlp.as_val()?))
        }
    }
}

impl Encodable for Action {
    fn rlp_append(&self, stream: &mut RlpStream) {
        match *self {
            Action::Create => stream.append_internal(&""),
            Action::Call(ref address) => stream.append_internal(address),
        };
    }
}

#[derive(
    Default,
    Debug,
    Clone,
    Eq,
    PartialEq,
    RlpEncodable,
    RlpDecodable,
    Serialize,
    Deserialize,
)]
pub struct NativeTransaction {
    /// Nonce.
    pub nonce: U256,
    /// Gas price.
    pub gas_price: U256,
    /// Gas paid up front for transaction execution.
    pub gas: U256,
    /// Action, can be either call or contract create.
    pub action: Action,
    /// Transferred value.
    pub value: U256,
    /// Maximum storage increasement in this execution.
    pub storage_limit: u64,
    /// The epoch height of the transaction. A transaction
    /// can only be packed between the epochs of [epoch_height -
    /// TRANSACTION_EPOCH_BOUND, epoch_height + TRANSACTION_EPOCH_BOUND]
    pub epoch_height: u64,
    /// The chain id of the transaction
    pub chain_id: u32,
    /// Transaction data.
    pub data: Bytes,
}

impl NativeTransaction {
    /// Specify the sender; this won't survive the serialize/deserialize
    /// process, but can be cloned.
    pub fn fake_sign(self, from: AddressWithSpace) -> SignedTransaction {
        // TODO: EVM core: for EVM tx
        SignedTransaction {
            transaction: TransactionWithSignature {
                transaction: TransactionWithSignatureSerializePart {
                    unsigned: Transaction::Native(self),
                    r: U256::one(),
                    s: U256::one(),
                    v: 0,
                },
                hash: H256::zero(),
                rlp_size: None,
            }
            .compute_hash(),
            sender: from.address,
            public: None,
        }
    }
}

#[derive(Default, Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct Eip155Transaction {
    /// Nonce.
    pub nonce: U256,
    /// Gas price.
    pub gas_price: U256,
    /// Gas paid up front for transaction execution.
    pub gas: U256,
    /// Action, can be either call or contract create.
    pub action: Action,
    /// Transferred value.
    pub value: U256,
    /// The chain id of the transaction
    pub chain_id: u32,
    /// Transaction data.
    pub data: Bytes,
}

impl Encodable for Eip155Transaction {
    fn rlp_append(&self, s: &mut RlpStream) {
        s.begin_list(9);
        s.append(&self.nonce);
        s.append(&self.gas_price);
        s.append(&self.gas);
        s.append(&self.action);
        s.append(&self.value);
        s.append(&self.data);
        s.append(&self.chain_id);
        s.append(&0u8);
        s.append(&0u8);
    }
}

impl Decodable for Eip155Transaction {
    fn decode(rlp: &Rlp) -> Result<Self, DecoderError> {
        if !(rlp.at(7)?.is_empty() && rlp.at(8)?.is_empty()) {
            return Err(DecoderError::Custom(
                "The last two items should be empty",
            ));
        }
        Ok(Self {
            nonce: rlp.val_at(0)?,
            gas_price: rlp.val_at(1)?,
            gas: rlp.val_at(2)?,
            action: rlp.val_at(3)?,
            value: rlp.val_at(4)?,
            chain_id: rlp.val_at(5)?,
            data: rlp.val_at(6)?,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Transaction {
    Native(NativeTransaction),
    Ethereum(Eip155Transaction),
}

impl Default for Transaction {
    fn default() -> Self { Transaction::Native(Default::default()) }
}

impl From<NativeTransaction> for Transaction {
    fn from(tx: NativeTransaction) -> Self { Self::Native(tx) }
}

impl From<Eip155Transaction> for Transaction {
    fn from(tx: Eip155Transaction) -> Self { Self::Ethereum(tx) }
}
impl Encodable for Transaction {
    fn rlp_append(&self, s: &mut RlpStream) {
        match self {
            Transaction::Native(tx) => {
                s.append(tx);
            }
            Transaction::Ethereum(tx) => {
                s.append(tx);
            }
        }
    }
}

impl Decodable for Transaction {
    fn decode(rlp: &Rlp) -> Result<Self, DecoderError> {
        if rlp.item_count()? != 9 {
            return Err(DecoderError::RlpInvalidLength);
        }
        let detected_eip155 = rlp.at(7)?.is_empty() && rlp.at(8)?.is_empty();
        let tx = if detected_eip155 {
            Transaction::Ethereum(Eip155Transaction::decode(rlp)?)
        } else {
            Transaction::Native(NativeTransaction::decode(rlp)?)
        };
        Ok(tx)
    }
}

macro_rules! access_common_ref {
    ($field: ident, $ty: ident) => {
        pub fn $field(&self) -> &$ty{
            match self {
                Transaction::Native(tx) => &tx.$field,
                Transaction::Ethereum(tx) => &tx.$field,
            }
        }
    };
}

macro_rules! access_common {
    ($field: ident, $ty: ident) => {
        pub fn $field(&self) -> $ty{
            match self {
                Transaction::Native(tx) => tx.$field,
                Transaction::Ethereum(tx) => tx.$field,
            }
        }
    };
}
impl Transaction {
    access_common_ref!(gas, U256);

    access_common_ref!(gas_price, U256);

    access_common_ref!(data, Bytes);

    access_common_ref!(nonce, U256);

    access_common_ref!(action, Action);

    access_common_ref!(value, U256);

    access_common!(chain_id, u32);
}

impl Transaction {
    // This function returns the hash value used in transaction signature. It is
    // different from transaction hash. The transaction hash also contains
    // signatures.
    pub fn signature_hash(&self) -> H256 {
        let mut s = RlpStream::new();
        s.append(self);
        keccak(s.as_raw())
    }

    pub fn space(&self) -> Space {
        match self {
            Transaction::Native(_) => Space::Native,
            Transaction::Ethereum(_) => Space::Ethereum,
        }
    }

    pub fn sign(self, secret: &Secret) -> SignedTransaction {
        let sig = ::keylib::sign(secret, &self.signature_hash())
            .expect("data is valid and context has signing capabilities; qed");
        let tx_with_sig = self.with_signature(sig);
        let public = tx_with_sig
            .recover_public()
            .expect("secret is valid so it's recoverable");
        SignedTransaction::new(public, tx_with_sig)
    }

    /// Signs the transaction with signature.
    pub fn with_signature(self, sig: Signature) -> TransactionWithSignature {
        TransactionWithSignature {
            transaction: TransactionWithSignatureSerializePart {
                unsigned: self,
                r: sig.r().into(),
                s: sig.s().into(),
                v: sig.v(),
            },
            hash: H256::zero(),
            rlp_size: None,
        }
        .compute_hash()
    }
}

impl MallocSizeOf for Transaction {
    fn size_of(&self, ops: &mut MallocSizeOfOps) -> usize {
        self.data().size_of(ops)
    }
}

/// Signed transaction information without verified signature.
#[derive(
    Debug,
    Clone,
    Eq,
    PartialEq,
    RlpEncodable,
    RlpDecodable,
    Serialize,
    Deserialize,
)]
pub struct TransactionWithSignatureSerializePart {
    /// Plain Transaction.
    pub unsigned: Transaction,
    /// The V field of the signature; helps describe which half of the curve
    /// our point falls in.
    pub v: u8,
    /// The R field of the signature; helps describe the point on the curve.
    pub r: U256,
    /// The S field of the signature; helps describe the point on the curve.
    pub s: U256,
}

impl Deref for TransactionWithSignatureSerializePart {
    type Target = Transaction;

    fn deref(&self) -> &Self::Target { &self.unsigned }
}

/// Signed transaction information without verified signature.
#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct TransactionWithSignature {
    /// Serialize part.
    pub transaction: TransactionWithSignatureSerializePart,
    /// Hash of the transaction
    #[serde(skip)]
    pub hash: H256,
    /// The transaction size when serialized in rlp
    #[serde(skip)]
    pub rlp_size: Option<usize>,
}

impl Deref for TransactionWithSignature {
    type Target = TransactionWithSignatureSerializePart;

    fn deref(&self) -> &Self::Target { &self.transaction }
}

impl Decodable for TransactionWithSignature {
    fn decode(d: &Rlp) -> Result<Self, DecoderError> {
        let hash = keccak(d.as_raw());
        let rlp_size = Some(d.as_raw().len());
        // Check item count of TransactionWithSignatureSerializePart
        if d.item_count()? != 4 {
            return Err(DecoderError::RlpIncorrectListLen);
        }
        let transaction = d.as_val()?;
        Ok(TransactionWithSignature {
            transaction,
            hash,
            rlp_size,
        })
    }
}

impl Encodable for TransactionWithSignature {
    fn rlp_append(&self, s: &mut RlpStream) {
        s.append_internal(&self.transaction);
    }
}

impl TransactionWithSignature {
    pub fn new_unsigned(tx: Transaction) -> Self {
        TransactionWithSignature {
            transaction: TransactionWithSignatureSerializePart {
                unsigned: tx,
                s: 0.into(),
                r: 0.into(),
                v: 0,
            },
            hash: Default::default(),
            rlp_size: None,
        }
    }

    /// Used to compute hash of created transactions
    fn compute_hash(mut self) -> TransactionWithSignature {
        let hash = keccak(&*self.rlp_bytes());
        self.hash = hash;
        self
    }

    /// Checks whether signature is empty.
    pub fn is_unsigned(&self) -> bool { self.r.is_zero() && self.s.is_zero() }

    /// Construct a signature object from the sig.
    pub fn signature(&self) -> Signature {
        let r: H256 = BigEndianHash::from_uint(&self.r);
        let s: H256 = BigEndianHash::from_uint(&self.s);
        Signature::from_rsv(&r, &s, self.v)
    }

    /// Checks whether the signature has a low 's' value.
    pub fn check_low_s(&self) -> Result<(), keylib::Error> {
        if !self.signature().is_low_s() {
            Err(keylib::Error::InvalidSignature)
        } else {
            Ok(())
        }
    }

    pub fn hash(&self) -> H256 { self.hash }

    /// Recovers the public key of the sender.
    pub fn recover_public(&self) -> Result<Public, keylib::Error> {
        Ok(recover(&self.signature(), &self.unsigned.signature_hash())?)
    }

    pub fn rlp_size(&self) -> usize {
        self.rlp_size.unwrap_or_else(|| self.rlp_bytes().len())
    }
}

impl MallocSizeOf for TransactionWithSignature {
    fn size_of(&self, ops: &mut MallocSizeOfOps) -> usize {
        self.unsigned.size_of(ops)
    }
}

/// A signed transaction with successfully recovered `sender`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SignedTransaction {
    pub transaction: TransactionWithSignature,
    pub sender: Address,
    pub public: Option<Public>,
}

impl Encodable for SignedTransaction {
    fn rlp_append(&self, s: &mut RlpStream) {
        s.begin_list(3);
        s.append(&self.transaction);
        s.append(&self.sender);
        s.append(&self.public);
    }
}

impl Decodable for SignedTransaction {
    fn decode(rlp: &Rlp) -> Result<Self, DecoderError> {
        Ok(SignedTransaction {
            transaction: rlp.val_at(0)?,
            sender: rlp.val_at(1)?,
            public: rlp.val_at(2)?,
        })
    }
}

impl Deref for SignedTransaction {
    type Target = TransactionWithSignature;

    fn deref(&self) -> &Self::Target { &self.transaction }
}

impl From<SignedTransaction> for TransactionWithSignature {
    fn from(tx: SignedTransaction) -> Self { tx.transaction }
}

impl SignedTransaction {
    /// Try to verify transaction and recover sender.
    pub fn new(public: Public, transaction: TransactionWithSignature) -> Self {
        if transaction.is_unsigned() {
            SignedTransaction {
                transaction,
                sender: UNSIGNED_SENDER,
                public: None,
            }
        } else {
            let sender = public_to_address(&public);
            SignedTransaction {
                transaction,
                sender,
                public: Some(public),
            }
        }
    }

    pub fn new_unsigned(transaction: TransactionWithSignature) -> Self {
        SignedTransaction {
            transaction,
            sender: UNSIGNED_SENDER,
            public: None,
        }
    }

    pub fn set_public(&mut self, public: Public) {
        self.sender = public_to_address(&public);
        self.public = Some(public);
    }

    /// Returns transaction sender.
    pub fn sender(&self) -> AddressWithSpace {
        self.sender.with_space(self.space())
    }

    pub fn nonce(&self) -> &U256 { self.transaction.nonce() }

    /// Checks if signature is empty.
    pub fn is_unsigned(&self) -> bool { self.transaction.is_unsigned() }

    pub fn hash(&self) -> H256 { self.transaction.hash() }

    pub fn gas(&self) -> &U256 { &self.transaction.gas() }

    pub fn gas_price(&self) -> &U256 { &self.transaction.gas_price() }

    pub fn gas_limit(&self) -> &U256 { &self.transaction.gas() }

    pub fn rlp_size(&self) -> usize { self.transaction.rlp_size() }

    pub fn public(&self) -> &Option<Public> { &self.public }

    pub fn verify_public(&self, skip: bool) -> Result<bool, keylib::Error> {
        if self.public.is_none() {
            return Ok(false);
        }

        if !skip {
            let public = self.public.unwrap();
            Ok(verify_public(
                &public,
                &self.signature(),
                &self.unsigned.signature_hash(),
            )?)
        } else {
            Ok(true)
        }
    }
}

impl MallocSizeOf for SignedTransaction {
    fn size_of(&self, ops: &mut MallocSizeOfOps) -> usize {
        self.transaction.size_of(ops)
    }
}
