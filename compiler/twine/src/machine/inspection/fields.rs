//! Borrowed data fields for embedding decoders. No labels/states are copied and
//! no values are forced. The borrow prevents evaluation while a view is live;
//! copied handles keep the ordinary session validation and forcing rules.
use super::*;

/// Non-forcing view of a record's names and field handles.
pub struct RecordFields<'a> {
    session: u64,
    fields: &'a BTreeMap<String, ThunkId>,
}
impl RecordFields<'_> {
    pub fn len(&self) -> usize {
        self.fields.len()
    }
    pub fn is_empty(&self) -> bool {
        self.fields.is_empty()
    }
    pub fn get(&self, name: &str) -> Option<ThunkHandle> {
        self.fields.get(name).map(|t| ThunkHandle {
            session: self.session,
            thunk: t.0,
        })
    }
    /// Fields in the same name order as `Session::children`.
    pub fn iter(&self) -> impl ExactSizeIterator<Item = (&str, ThunkHandle)> + DoubleEndedIterator {
        self.fields.iter().map(|(name, t)| {
            (
                name.as_str(),
                ThunkHandle {
                    session: self.session,
                    thunk: t.0,
                },
            )
        })
    }
}

/// Non-forcing view of constructor or tuple field handles.
pub struct IndexedFields<'a> {
    session: u64,
    fields: &'a [ThunkId],
}
impl IndexedFields<'_> {
    pub fn len(&self) -> usize {
        self.fields.len()
    }
    pub fn is_empty(&self) -> bool {
        self.fields.is_empty()
    }
    pub fn get(&self, index: usize) -> Option<ThunkHandle> {
        self.fields.get(index).map(|t| ThunkHandle {
            session: self.session,
            thunk: t.0,
        })
    }
    pub fn iter(&self) -> impl ExactSizeIterator<Item = ThunkHandle> + DoubleEndedIterator + '_ {
        self.fields.iter().map(|t| ThunkHandle {
            session: self.session,
            thunk: t.0,
        })
    }
}

impl ValueReader<'_> {
    /// Borrow an evaluated record's field handles without allocation or forcing.
    /// Other value kinds return None; foreign/abandoned sessions return an error.
    pub fn record_fields(&self, value: ValueHandle) -> Result<Option<RecordFields<'_>>, Outcome> {
        Ok(match self.session.read_value(value)? {
            Value::Record(fields) => Some(RecordFields {
                session: self.session.id,
                fields,
            }),
            _ => None,
        })
    }
    /// Borrow constructor or tuple field handles in positional order. Literals,
    /// records, functions, dictionaries, native values and IO are opaque here.
    pub fn indexed_fields(&self, value: ValueHandle) -> Result<Option<IndexedFields<'_>>, Outcome> {
        Ok(match self.session.read_value(value)? {
            Value::Ctor(_, fields) | Value::DecimalRatio(_, _, fields) | Value::Tuple(fields) => {
                Some(IndexedFields {
                    session: self.session.id,
                    fields,
                })
            }
            _ => None,
        })
    }
}
