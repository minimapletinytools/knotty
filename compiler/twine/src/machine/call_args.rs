//! Most applications supply one argument or the two operands of an operator.
//! Keep these in the continuation until a value actually needs to own a vector.
use super::ThunkId;

pub(super) enum CallArgs {
    Empty,
    One([ThunkId; 1]),
    Two([ThunkId; 2]),
    Many(Vec<ThunkId>),
}

impl CallArgs {
    pub fn as_slice(&self) -> &[ThunkId] {
        match self {
            Self::Empty => &[],
            Self::One(args) => args,
            Self::Two(args) => args,
            Self::Many(args) => args,
        }
    }

    pub fn take_first(&mut self) -> ThunkId {
        match self {
            Self::Empty => unreachable!("empty application"),
            Self::One([first]) => {
                let result = *first;
                *self = Self::Empty;
                result
            }
            Self::Two([first, second]) => {
                let result = *first;
                *self = Self::One([*second]);
                result
            }
            Self::Many(args) => args.remove(0),
        }
    }

    pub fn into_vec(self) -> Vec<ThunkId> {
        match self {
            Self::Many(args) => args,
            other => other.as_slice().to_vec(),
        }
    }
}
