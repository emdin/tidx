# Tempo Protocol Specifics for AK47 Indexer

This document outlines the key differences between Tempo and Ethereum that are relevant to indexer design.

## 1. Transaction Structure

### Tempo Transaction Type (0x76)

Tempo introduces a new EIP-2718 transaction type with ID `0x76` that extends Ethereum transactions with additional features:

```rust
pub struct TempoTransaction {
    // Standard EIP-1559 fields
    chain_id: ChainId,
    max_priority_fee_per_gas: u128,
    max_fee_per_gas: u128,
    gas_limit: u64,
    access_list: AccessList,

    // TEMPO-SPECIFIC: Batch calls (replaces single to/value/input)
    calls: Vec<Call>,              // Multiple calls executed atomically

    // TEMPO-SPECIFIC: 2D Nonce System
    nonce_key: U256,               // Nonce key (0 = protocol nonce, >0 = user nonces)
    nonce: u64,                    // Nonce value for the key

    // TEMPO-SPECIFIC: Optional features
    fee_token: Option<Address>,                    // Fee token preference
    fee_payer_signature: Option<Signature>,        // Sponsored transactions
    valid_before: Option<u64>,                     // Transaction expiration timestamp
    valid_after: Option<u64>,                      // Transaction activation timestamp
    key_authorization: Option<SignedKeyAuthorization>, // Access key provisioning
    tempo_authorization_list: Vec<TempoSignedAuthorization>, // EIP-7702 style with AA sigs
}

pub struct Call {
    to: TxKind,      // Address or Create
    value: U256,
    input: Bytes,
}
```

### Key Differences from Ethereum

| Feature | Ethereum | Tempo |
|---------|----------|-------|
| Transaction type | 0x00-0x04 | 0x00-0x04 + **0x76** |
| Calls per tx | 1 | **Multiple (batched)** |
| Nonce | Single sequential | **2D nonce (key + value)** |
| Fee token | ETH only | **Any USD TIP-20 token** |
| Fee payer | Sender only | **Sponsor support** |
| Signature types | secp256k1 only | **secp256k1 + P256 + WebAuthn** |
| Time constraints | None | **valid_before / valid_after** |

### Supported Transaction Types

```rust
pub enum TempoTxEnvelope {
    Legacy(Signed<TxLegacy>),      // 0x00
    Eip2930(Signed<TxEip2930>),    // 0x01
    Eip1559(Signed<TxEip1559>),    // 0x02
    Eip7702(Signed<TxEip7702>),    // 0x04
    AA(AASigned),                  // 0x76 - Tempo transaction
}
```

**Note:** EIP-4844 (blob transactions) are NOT supported.

---

## 2. Signature Types

Tempo supports multiple signature schemes:

### secp256k1 (65 bytes)
- Standard Ethereum signature
- `r (32) || s (32) || v (1)`
- No type prefix

### P256 (130 bytes)
- Type prefix: `0x01`
- `typeId (1) || r (32) || s (32) || pub_key_x (32) || pub_key_y (32) || pre_hash (1)`

### WebAuthn (variable, max 2KB)
- Type prefix: `0x02`
- `typeId (1) || authenticatorData || clientDataJSON || r (32) || s (32) || pub_key_x (32) || pub_key_y (32)`

### Keychain (variable)
- Type prefix: `0x03`
- `typeId (1) || user_address (20) || inner_signature`
- Used for access keys signing on behalf of root account

### Address Derivation

**secp256k1:**
```solidity
address(uint160(uint256(keccak256(abi.encode(x, y)))))
```

**P256/WebAuthn:**
```solidity
address(uint160(uint256(keccak256(abi.encodePacked(pubKeyX, pubKeyY)))))
```

---

## 3. 2D Nonce System

Tempo uses a parallelizable nonce system:

- **Protocol nonce (key = 0):** Standard sequential nonce, stored in account state
- **User nonces (key > 0):** Parallel execution nonces, stored in Nonce precompile at `0x4E4F4E4345000000000000000000000000000000`

### Reserved Nonce Keys

Nonce keys with prefix `0x5b` are reserved for **subblock transactions**:
- Format: `(0x5b << 248) + (validatorPubKey120 << 128) + x`
- Each validator has a dedicated nonce space identified by their public key

### Storage Layout

For user nonces at the Nonce precompile:
- Storage key: `keccak256(abi.encode(account_address, nonce_key))`
- Storage value: `nonce (uint64)`

---

## 4. Time Windows

Tempo transactions can specify execution time constraints:

- `valid_before: Option<u64>` - Transaction expires at this timestamp
- `valid_after: Option<u64>` - Transaction only valid after this timestamp

**Validation:** `valid_before > valid_after` must hold if both are set.

---

## 5. Batch Calls

A single Tempo transaction can execute multiple calls atomically:

```rust
pub struct Call {
    to: TxKind,    // TxKind::Call(address) or TxKind::Create
    value: U256,
    input: Bytes,
}
```

**Constraints:**
- `calls` list cannot be empty
- Only the **first call** can be a CREATE (contract deployment)
- CREATE calls are forbidden when `tempo_authorization_list` is non-empty
- All calls execute atomically (all succeed or all revert)

---

## 6. Fee System

### Fee Token

Fees can be paid in any TIP-20 token whose currency is USD. Token preference hierarchy:

1. **Transaction level:** `fee_token` field
2. **Account level:** Set via FeeManager precompile
3. **TIP-20 contract:** Auto-detect from `transfer/transferWithMemo/distributeReward` calls
4. **Stablecoin DEX:** Auto-detect from swap calls
5. **Fallback:** pathUSD

### Fee Payer (Sponsorship)

If `fee_payer_signature` is present:
- Fee payer signs with magic byte `0x78` (not `0x76`)
- Fee payer signature includes: sender_address + fee_token
- Sender signature skips `fee_token` when fee payer is present

**Fee payer hash:**
```
keccak256(0x78 || rlp([chain_id, ..., fee_token, sender_address]))
```

### Fee Units

Fees are in **USD per 10^18 gas**. Since TIP-20 has 6 decimals:
```
fee = ceil(base_fee * gas_used / 10^12)
```

---

## 7. Key Authorization (Access Keys)

Root accounts can provision scoped access keys with spending limits.

### KeyAuthorization Structure

```rust
pub struct KeyAuthorization {
    chain_id: u64,                    // 0 = valid on any chain
    key_type: SignatureType,          // Secp256k1/P256/WebAuthn
    key_id: Address,                  // Address derived from public key
    expiry: Option<u64>,              // Unix timestamp (None = never expires)
    limits: Option<Vec<TokenLimit>>,  // TIP-20 spending limits
}

pub struct TokenLimit {
    token: Address,    // TIP-20 token address
    limit: U256,       // Maximum spending amount
}
```

### AccountKeychain Precompile

Address: `0xaAAAaaAA00000000000000000000000000000000`

Manages authorized access keys:
- `authorizeKey()` - Add new access key (root key only)
- `revokeKey()` - Remove access key (root key only)
- `updateSpendingLimit()` - Update token limits (root key only)
- `getKey()` - Query key info
- `getRemainingLimit()` - Query remaining spending limit

---

## 8. Receipt Structure

Tempo receipts extend Ethereum receipts:

```rust
pub struct TempoTransactionReceipt {
    // Standard receipt fields
    inner: TransactionReceipt<...>,
    
    // TEMPO-SPECIFIC
    fee_token: Option<Address>,  // Token used for fees (None if free)
    fee_payer: Address,          // Address that paid fees
}
```

---

## 9. Block Structure

### Block Layout

```
transactions = [proposer_transactions] | [subblock_transactions] | [gas_incentive_transactions] | [system_transaction]
```

### Subblocks

Non-proposing validators can include transactions via signed subblocks:

```
subblock = rlp([version, parent_hash, fee_recipient, [transactions], signature])
```

- Each validator gets a reserved nonce space (prefix `0x5b`)
- Subblock transactions are contiguous in the block
- Magic byte for signature: `0x78`

### System Transaction

**Last transaction** in every block:
- From: `0x0000...0000` (zero address)
- To: `0x0000...0000`
- Signature: `r=0, s=0, yParity=false`
- Gas: 0, Value: 0, Nonce: 0
- Calldata: `rlp([[version, validator_pubkey, fee_recipient, signature], ...])`

Used to identify the transaction as a system transaction:
```rust
pub const TEMPO_SYSTEM_TX_SIGNATURE: Signature = Signature::new(U256::ZERO, U256::ZERO, false);
```

### Header Field

New header field:
```
shared_gas_limit  // Total gas for subblocks + gas incentive transactions
```

---

## 10. Payment Classification

Transactions targeting TIP-20 tokens are classified as "payments":

```rust
const TIP20_PAYMENT_PREFIX: [u8; 12] = hex!("20C000000000000000000000");

fn is_payment(&self) -> bool {
    // Check if `to` address starts with TIP20_PAYMENT_PREFIX
    to.starts_with(&TIP20_PAYMENT_PREFIX)
}
```

For Tempo transactions: **all calls** must target TIP-20 addresses.

---

## 11. Gas Costs

### Signature Verification

| Type | Base Cost | Notes |
|------|-----------|-------|
| secp256k1 | 21,000 | Standard |
| P256 | 26,000 | +5,000 for P256 verification |
| WebAuthn | 26,000 + calldata | Variable for clientDataJSON |
| Keychain | inner + 3,000 | Key validation overhead |

### Nonce Key Costs

| Case | Additional Gas |
|------|----------------|
| Protocol nonce (key=0) | 0 |
| Existing user key (nonce>0) | 5,000 |
| New user key (nonce=0) | 22,100 |

---

## 12. Indexer Considerations

### New Fields to Index

For Tempo transactions (type 0x76):
- `calls[]` - Array of calls instead of single to/value/input
- `nonce_key` - 2D nonce key
- `fee_token` - Optional fee token address
- `fee_payer` - Resolved fee payer (from receipt)
- `valid_before` / `valid_after` - Time constraints
- `key_authorization` - Access key provisioning
- `tempo_authorization_list` - EIP-7702 style authorizations

### Receipt Extensions

- `fee_token: Option<Address>` - Token used for fees
- `fee_payer: Address` - Who paid the fees

### Special Transaction Detection

- **System transaction:** `signature == (0, 0, false)` and last in block
- **Subblock transaction:** `nonce_key` starts with `0x5b`
- **Payment transaction:** `to` address starts with `0x20C0...`

### Sender Recovery

Use `SignerRecoverable::recover_signer()` which handles:
- Standard signature recovery for secp256k1
- P256 address derivation from public key
- WebAuthn verification and address derivation
- Keychain signature unwrapping
- System transaction detection (returns `Address::ZERO`)
