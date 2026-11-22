//! Non-owning witnesses for embedding-side derived data. A numeric proof ID
//! alone is not sufficient across independently allocating sibling revisions.
use super::*;
use std::sync::Weak;

#[derive(Clone)]
pub(in crate::machine) struct Witness {
    pub(in crate::machine) proof: Proof,
    record: Weak<Certificate>,
}
impl Witness {
    pub(in crate::machine) fn weak_bytes() -> usize {
        // Weak references keep the allocation header after its payload is dropped.
        std::mem::size_of::<Certificate>() + 2 * std::mem::size_of::<usize>()
    }
}
impl Store {
    pub(in crate::machine) fn witness(&self, proof: Proof) -> Option<Witness> {
        Some(Witness {
            proof,
            record: Arc::downgrade(self.records.get(proof.id?)?),
        })
    }
    pub(in crate::machine) fn owns_witness(&self, witness: &Witness) -> bool {
        witness
            .proof
            .id
            .and_then(|id| self.records.get(id))
            .is_some_and(|record| Weak::ptr_eq(&Arc::downgrade(record), &witness.record))
    }
}
