use std::fmt;
use std::marker::PhantomData;
use std::ops::Deref;

use serde::de::{self, IgnoredAny, SeqAccess, Visitor};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// A list that never holds more than `N` items, including while it is being
/// deserialized: input with more items fails at item `N + 1`, which is skipped
/// rather than built. Every collection in the contract uses this, so the size
/// of a valid message has a fixed upper bound.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct BoundedVec<T, const N: usize>(Vec<T>);

/// A collection was given more items than its bound allows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TooManyItems {
    pub max: usize,
    pub len: usize,
}

impl fmt::Display for TooManyItems {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} items given, at most {} allowed", self.len, self.max)
    }
}

impl std::error::Error for TooManyItems {}

impl<T, const N: usize> BoundedVec<T, N> {
    pub const MAX: usize = N;

    pub const fn new() -> Self {
        Self(Vec::new())
    }

    pub fn into_vec(self) -> Vec<T> {
        self.0
    }
}

impl<T, const N: usize> Default for BoundedVec<T, N> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T, const N: usize> Deref for BoundedVec<T, N> {
    type Target = [T];

    fn deref(&self) -> &[T] {
        &self.0
    }
}

impl<'a, T, const N: usize> IntoIterator for &'a BoundedVec<T, N> {
    type Item = &'a T;
    type IntoIter = std::slice::Iter<'a, T>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.iter()
    }
}

impl<T, const N: usize> TryFrom<Vec<T>> for BoundedVec<T, N> {
    type Error = TooManyItems;

    fn try_from(items: Vec<T>) -> Result<Self, TooManyItems> {
        if items.len() > N {
            return Err(TooManyItems {
                max: N,
                len: items.len(),
            });
        }
        Ok(Self(items))
    }
}

impl<T: Serialize, const N: usize> Serialize for BoundedVec<T, N> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.0.serialize(serializer)
    }
}

impl<'de, T: Deserialize<'de>, const N: usize> Deserialize<'de> for BoundedVec<T, N> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_seq(BoundedVisitor::<T, N>(PhantomData))
    }
}

struct BoundedVisitor<T, const N: usize>(PhantomData<T>);

impl<'de, T: Deserialize<'de>, const N: usize> Visitor<'de> for BoundedVisitor<T, N> {
    type Value = BoundedVec<T, N>;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "a list of at most {N} items")
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
        // The size hint comes from the input, so it only ever lowers the
        // allocation below the bound.
        let mut items = Vec::with_capacity(seq.size_hint().unwrap_or(0).min(N));
        while items.len() < N {
            match seq.next_element()? {
                Some(item) => items.push(item),
                None => return Ok(BoundedVec(items)),
            }
        }
        if seq.next_element::<IgnoredAny>()?.is_some() {
            return Err(de::Error::invalid_length(N + 1, &self));
        }
        Ok(BoundedVec(items))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    type Three = BoundedVec<u8, 3>;

    #[test]
    fn construction_enforces_the_bound() {
        assert_eq!(Three::try_from(vec![1, 2, 3]).unwrap().len(), 3);
        assert_eq!(
            Three::try_from(vec![1, 2, 3, 4]).unwrap_err(),
            TooManyItems { max: 3, len: 4 }
        );
    }

    #[test]
    fn deserialization_enforces_the_bound() {
        assert_eq!(serde_json::from_str::<Three>("[]").unwrap().len(), 0);
        assert_eq!(serde_json::from_str::<Three>("[1,2,3]").unwrap().len(), 3);

        let err = serde_json::from_str::<Three>("[1,2,3,4]").unwrap_err();
        assert!(err.to_string().contains("at most 3 items"), "{err}");
    }

    #[test]
    fn extra_items_are_rejected_even_when_they_would_not_parse_as_items() {
        // Item 4 is not a u8; the bound fails first because it is never built.
        let err = serde_json::from_str::<Three>(r#"[1,2,3,{"x":[1,2]}]"#).unwrap_err();
        assert!(err.to_string().contains("at most 3 items"), "{err}");
    }

    #[test]
    fn serializes_as_a_plain_list() {
        let v = Three::try_from(vec![7, 8]).unwrap();
        assert_eq!(serde_json::to_string(&v).unwrap(), "[7,8]");
    }
}
