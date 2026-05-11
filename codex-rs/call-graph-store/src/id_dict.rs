use sled::{Db, Tree};

use crate::error::CallGraphStoreError;
use crate::schema::StringId;

const TREE_DICT_FORWARD: &str = "id_dict";
const TREE_DICT_REVERSE: &str = "id_dict_rev";
const COUNTER_KEY: &[u8] = b"__next_string_id";

pub struct StringDict {
    forward: Tree,
    reverse: Tree,
    counters: Tree,
}

impl StringDict {
    pub fn open(db: &Db, counters: Tree) -> Result<Self, CallGraphStoreError> {
        Ok(Self {
            forward: db.open_tree(TREE_DICT_FORWARD)?,
            reverse: db.open_tree(TREE_DICT_REVERSE)?,
            counters,
        })
    }

    pub fn intern(&self, s: &str) -> Result<StringId, CallGraphStoreError> {
        let key = s.as_bytes();
        if let Some(existing) = self.reverse.get(key)? {
            return Ok(decode_u32(&existing));
        }
        let id = self.allocate_id()?;
        let id_bytes = id.to_le_bytes();
        self.reverse.insert(key, &id_bytes[..])?;
        self.forward.insert(&id_bytes[..], s.as_bytes())?;
        Ok(id)
    }

    pub fn lookup(&self, id: StringId) -> Result<Option<String>, CallGraphStoreError> {
        let id_bytes = id.to_le_bytes();
        let Some(raw) = self.forward.get(id_bytes)? else {
            return Ok(None);
        };
        let s = std::str::from_utf8(&raw)
            .map_err(|err| CallGraphStoreError::Integrity(format!("non-utf8 string for id {id}: {err}")))?
            .to_string();
        Ok(Some(s))
    }

    pub fn lookup_required(&self, id: StringId) -> Result<String, CallGraphStoreError> {
        self.lookup(id)?.ok_or(CallGraphStoreError::UnknownStringId(id))
    }

    pub fn lookup_id(&self, s: &str) -> Result<Option<StringId>, CallGraphStoreError> {
        Ok(self
            .reverse
            .get(s.as_bytes())?
            .map(|raw| decode_u32(&raw)))
    }

    pub fn len(&self) -> usize {
        self.forward.len()
    }

    fn allocate_id(&self) -> Result<StringId, CallGraphStoreError> {
        let mut current = self
            .counters
            .get(COUNTER_KEY)?
            .map(|raw| decode_u32(&raw))
            .unwrap_or(0);
        loop {
            let next = current
                .checked_add(1)
                .ok_or(CallGraphStoreError::IdExhausted { what: "string id" })?;
            let next_bytes = next.to_le_bytes();
            let prev_bytes = current.to_le_bytes();
            let cas_result = self.counters.compare_and_swap(
                COUNTER_KEY,
                if current == 0 {
                    None
                } else {
                    Some(&prev_bytes[..])
                },
                Some(&next_bytes[..]),
            )?;
            match cas_result {
                Ok(()) => return Ok(current),
                Err(actual) => {
                    current = actual
                        .current
                        .as_ref()
                        .map(|raw| decode_u32(raw))
                        .unwrap_or(0);
                }
            }
        }
    }
}

pub(crate) fn decode_u32(raw: &[u8]) -> u32 {
    let mut buf = [0u8; 4];
    buf.copy_from_slice(&raw[..4]);
    u32::from_le_bytes(buf)
}

pub(crate) fn decode_u32_pair(raw: &[u8]) -> (u32, u32) {
    let mut buf = [0u8; 4];
    buf.copy_from_slice(&raw[..4]);
    let first = u32::from_le_bytes(buf);
    buf.copy_from_slice(&raw[4..8]);
    (first, u32::from_le_bytes(buf))
}

pub(crate) fn encode_u32(value: u32) -> [u8; 4] {
    value.to_le_bytes()
}

pub(crate) fn encode_u32_pair(a: u32, b: u32) -> [u8; 8] {
    let mut buf = [0u8; 8];
    buf[..4].copy_from_slice(&a.to_le_bytes());
    buf[4..].copy_from_slice(&b.to_le_bytes());
    buf
}
