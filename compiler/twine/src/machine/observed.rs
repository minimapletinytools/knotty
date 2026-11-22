//! Exact keys for small, already evaluated data. No forcing, user Eq, hashing
//! approximation, or closure/native-value comparison participates in this proof.
use super::*;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ObservedDataKey(Vec<u8>);
impl ObservedDataKey {
    pub fn byte_len(&self) -> usize {
        self.0.len()
    }
}

struct Bytes {
    data: Vec<u8>,
    limit: usize,
}
impl Bytes {
    fn put(&mut self, bytes: &[u8]) -> Option<()> {
        if bytes.len() > self.limit.saturating_sub(self.data.len()) {
            return None;
        }
        self.data.extend_from_slice(bytes);
        Some(())
    }
    fn size(&mut self, size: usize) -> Option<()> {
        self.put(&(size as u64).to_le_bytes())
    }
    fn text(&mut self, text: &str) -> Option<()> {
        self.size(text.len())?;
        self.put(text.as_bytes())
    }
}

impl Session {
    /// Representation equality of fully evaluated pure data, not language Eq.
    /// Float bits (including signed zero/NaN payloads) and constructor identities
    /// remain exact. Lazy/failed/cyclic/opaque/oversized values return None.
    /// Inspecting never evaluates code, allocates runtime values or runs IO.
    pub fn observed_data_key(
        &self,
        handle: ThunkHandle,
        nodes: usize,
        bytes: usize,
    ) -> Result<Option<ObservedDataKey>, Outcome> {
        self.validate_thunk(handle)?;
        let mut pending = vec![ThunkId(handle.thunk)];
        let mut remaining = nodes;
        let mut key = Bytes {
            data: vec![],
            limit: bytes,
        };
        while let Some(thunk) = pending.pop() {
            if self.limits.cancellation.load(Ordering::Relaxed) {
                return Err(Outcome::Cancelled);
            }
            if remaining == 0 {
                return Ok(None);
            }
            remaining -= 1;
            let ThunkState::Done(Ok(value)) = *self.cell_state(thunk) else {
                return Ok(None);
            };
            let encoded = (|| {
                let children: &[ThunkId] = match &self.values[value.value] {
                    Value::Lit(literal) => {
                        match literal {
                            Literal::Int(n) => {
                                key.put(&[0])?;
                                key.put(&n.to_le_bytes())?;
                            }
                            Literal::Float(n) => {
                                key.put(&[1])?;
                                key.put(&n.to_bits().to_le_bytes())?;
                            }
                            Literal::Integer(n) => {
                                if n.bits().div_ceil(8)
                                    > key.limit.saturating_sub(key.data.len()) as u64
                                {
                                    return None;
                                }
                                let (sign, bytes) = n.to_bytes_le();
                                key.put(&[
                                    2,
                                    match sign {
                                        num_bigint::Sign::Minus => 0,
                                        num_bigint::Sign::NoSign => 1,
                                        num_bigint::Sign::Plus => 2,
                                    },
                                ])?;
                                key.size(bytes.len())?;
                                key.put(&bytes)?;
                            }
                            Literal::String(text) => {
                                key.put(&[3])?;
                                key.text(text)?;
                            }
                            Literal::Unit => {
                                key.put(&[4])?;
                            }
                        }
                        &[]
                    }
                    Value::Ctor(name, fields) => {
                        key.put(&[5])?;
                        key.size(name.module.len())?;
                        for part in &name.module {
                            key.text(part)?;
                        }
                        key.text(&name.name)?;
                        key.size(fields.len())?;
                        fields
                    }
                    Value::Tuple(fields) => {
                        key.put(&[6])?;
                        key.size(fields.len())?;
                        fields
                    }
                    Value::Record(fields) => {
                        if fields.len() > remaining.saturating_sub(pending.len()) {
                            return None;
                        }
                        key.put(&[7])?;
                        key.size(fields.len())?;
                        for name in fields.keys() {
                            key.text(name)?;
                        }
                        pending.extend(fields.values().rev().copied());
                        return Some(());
                    }
                    _ => return None,
                };
                if children.len() > remaining.saturating_sub(pending.len()) {
                    return None;
                }
                pending.extend(children.iter().rev().copied());
                Some(())
            })();
            if encoded.is_none() {
                return Ok(None);
            }
        }
        Ok(Some(ObservedDataKey(key.data)))
    }
}
