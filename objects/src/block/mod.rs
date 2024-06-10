use alloc::{collections::BTreeSet, string::ToString, vec::Vec};

use super::{Digest, Felt, Hasher, MAX_BATCHES_PER_BLOCK, MAX_NOTES_PER_BATCH, ZERO};

mod header;
pub use header::BlockHeader;
mod note_tree;
pub use note_tree::{BlockNoteIndex, BlockNoteTree};

use crate::{
    accounts::{delta::AccountUpdateDetails, AccountId},
    errors::BlockError,
    notes::Nullifier,
    transaction::{OutputNote, TransactionId},
    utils::{ByteReader, ByteWriter, Deserializable, DeserializationError, Serializable},
};

pub type NoteBatch = Vec<OutputNote>;

// BLOCK
// ================================================================================================

/// A block in the Miden chain.
///
/// A block contains information resulting from executing a set of transactions against the chain
/// state defined by the previous block. It consists of 3 main components:
/// - A set of change descriptors for all accounts updated in this block. For private accounts,
///   the block contains only the new account state hashes; for public accounts, the block also
///   contains a set of state deltas which can be applied to the previous account state to get the
///   new account state.
/// - A set of new notes created in this block. For private notes, the block contains only note IDs
///   and note metadata; for public notes, full note details are recorded.
/// - A set of new nullifiers created for all notes that were consumed in the block.
///
/// In addition to the above components, a block also contains a block header which contains
/// commitments to the new state of the chain as well as a ZK proof attesting that a set of valid
/// transactions was executed to transition the chain into the state described by this block (the
/// ZK proof part is not yet implemented).
#[derive(Debug, Clone)]
pub struct Block {
    /// Block header.
    header: BlockHeader,

    /// Account updates for the block.
    updated_accounts: Vec<BlockAccountUpdate>,

    /// Note batches created in transactions in the block.
    created_notes: Vec<NoteBatch>,

    /// Nullifiers produced in transactions in the block.
    created_nullifiers: Vec<Nullifier>,
    //
    // TODO: add zk proof
}

impl Block {
    /// Returns a new [Block] instantiated from the provided components.
    ///
    /// Note: consistency of the provided components is not validated.
    pub fn new(
        header: BlockHeader,
        updated_accounts: Vec<BlockAccountUpdate>,
        created_notes: Vec<NoteBatch>,
        created_nullifiers: Vec<Nullifier>,
    ) -> Result<Self, BlockError> {
        let block = Self {
            header,
            updated_accounts,
            created_notes,
            created_nullifiers,
        };

        block.validate()?;

        Ok(block)
    }

    /// Returns a commitment to this block.
    pub fn hash(&self) -> Digest {
        self.header.hash()
    }

    /// Returns the header of this block.
    pub fn header(&self) -> BlockHeader {
        self.header
    }

    /// Returns a set of account update descriptions for all accounts updated in this block.
    pub fn updated_accounts(&self) -> &[BlockAccountUpdate] {
        &self.updated_accounts
    }

    /// Returns a set of note batches containing all notes created in this block.
    pub fn created_notes(&self) -> &[NoteBatch] {
        &self.created_notes
    }

    /// Returns an iterator over all notes created in this block.
    ///
    /// Each note is accompanied by a corresponding index specifying where the note is located
    /// in the blocks note tree.
    pub fn notes(&self) -> impl Iterator<Item = (BlockNoteIndex, &OutputNote)> {
        self.created_notes.iter().enumerate().flat_map(|(batch_idx, notes)| {
            notes.iter().enumerate().map(move |(note_idx_in_batch, note)| {
                (BlockNoteIndex::new(batch_idx, note_idx_in_batch), note)
            })
        })
    }

    /// Returns a note tree containing all notes created in this block.
    pub fn build_note_tree(&self) -> BlockNoteTree {
        let entries = self
            .notes()
            .map(|(note_index, note)| (note_index, note.id().into(), *note.metadata()));

        BlockNoteTree::with_entries(entries)
            .expect("Something went wrong: block is invalid, but passed or skipped validation")
    }

    /// Returns a set of nullifiers for all notes consumed in the block.
    pub fn created_nullifiers(&self) -> &[Nullifier] {
        &self.created_nullifiers
    }

    /// Returns an iterator over all transactions which affected accounts in the block with corresponding account IDs.
    pub fn transactions(&self) -> impl Iterator<Item = (TransactionId, AccountId)> + '_ {
        self.updated_accounts.iter().flat_map(|update| {
            update
                .transactions
                .iter()
                .map(|transaction_id| (*transaction_id, update.account_id))
        })
    }

    /// Computes a commitment to a set of IDs of transactions which affected accounts in this block.
    pub fn compute_tx_hash(
        updated_accounts: impl Iterator<Item = (TransactionId, AccountId)>,
    ) -> Digest {
        let mut elements = vec![];
        for (transaction_id, account_id) in updated_accounts {
            elements.extend_from_slice(&[account_id.into(), ZERO, ZERO, ZERO]);
            elements.extend_from_slice(transaction_id.as_elements());
        }

        Hasher::hash_elements(&elements)
    }

    // HELPER METHODS
    // --------------------------------------------------------------------------------------------

    fn validate(&self) -> Result<(), BlockError> {
        let batch_count = self.created_notes.len();
        if batch_count > MAX_BATCHES_PER_BLOCK {
            return Err(BlockError::TooManyTransactionBatches(batch_count));
        }

        for batch in self.created_notes.iter() {
            if batch.len() > MAX_NOTES_PER_BATCH {
                return Err(BlockError::TooManyNotesInBatch(batch.len()));
            }
        }

        let mut notes = BTreeSet::new();
        for batch in self.created_notes.iter() {
            for note in batch.iter() {
                if !notes.insert(note.id()) {
                    return Err(BlockError::DuplicateNoteFound(note.id()));
                }
            }
        }

        Ok(())
    }
}

impl Serializable for Block {
    fn write_into<W: ByteWriter>(&self, target: &mut W) {
        self.header.write_into(target);
        self.updated_accounts.write_into(target);
        self.created_notes.write_into(target);
        self.created_nullifiers.write_into(target);
    }
}

impl Deserializable for Block {
    fn read_from<R: ByteReader>(source: &mut R) -> Result<Self, DeserializationError> {
        let block = Self {
            header: BlockHeader::read_from(source)?,
            updated_accounts: <Vec<BlockAccountUpdate>>::read_from(source)?,
            created_notes: <Vec<NoteBatch>>::read_from(source)?,
            created_nullifiers: <Vec<Nullifier>>::read_from(source)?,
        };

        block
            .validate()
            .map_err(|err| DeserializationError::InvalidValue(err.to_string()))?;

        Ok(block)
    }
}

// BLOCK ACCOUNT UPDATE
// ================================================================================================

/// Describes the changes made to an account state resulting from executing transactions contained
/// in a block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlockAccountUpdate {
    /// ID of the updated account.
    account_id: AccountId,

    /// Hash of the new state of the account.
    new_state_hash: Digest,

    /// A set of changes which can be applied to the previous account state (i.e., the state as of
    /// the last block) to get the new account state. For private accounts, this is set to
    /// [AccountUpdateDetails::Private].
    details: AccountUpdateDetails,

    /// IDs of all transactions in the block that updated the account.
    transactions: Vec<TransactionId>,
}

impl BlockAccountUpdate {
    /// Returns a new [BlockAccountUpdate] instantiated from the specified components.
    pub const fn new(
        account_id: AccountId,
        new_state_hash: Digest,
        details: AccountUpdateDetails,
        transactions: Vec<TransactionId>,
    ) -> Self {
        Self {
            account_id,
            new_state_hash,
            details,
            transactions,
        }
    }

    /// Returns the ID of the updated account.
    pub fn account_id(&self) -> AccountId {
        self.account_id
    }

    /// Returns the hash of the new account state.
    pub fn new_state_hash(&self) -> Digest {
        self.new_state_hash
    }

    /// Returns the description of the updates for public accounts.
    ///
    /// These descriptions can be used to build the new account state from the previous account
    /// state.
    pub fn details(&self) -> &AccountUpdateDetails {
        &self.details
    }

    /// Returns the IDs of all transactions in the block that updated the account.
    pub fn transactions(&self) -> &[TransactionId] {
        &self.transactions
    }

    /// Returns `true` if the account update details are for private account.
    pub fn is_private(&self) -> bool {
        self.details.is_private()
    }
}

impl Serializable for BlockAccountUpdate {
    fn write_into<W: ByteWriter>(&self, target: &mut W) {
        self.account_id.write_into(target);
        self.new_state_hash.write_into(target);
        self.details.write_into(target);
        self.transactions.write_into(target);
    }
}

impl Deserializable for BlockAccountUpdate {
    fn read_from<R: ByteReader>(source: &mut R) -> Result<Self, DeserializationError> {
        Ok(Self {
            account_id: AccountId::read_from(source)?,
            new_state_hash: Digest::read_from(source)?,
            details: AccountUpdateDetails::read_from(source)?,
            transactions: Vec::<TransactionId>::read_from(source)?,
        })
    }
}
