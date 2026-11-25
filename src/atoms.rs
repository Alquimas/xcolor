use anyhow::{anyhow, Result};
use lazy_static::*;
use std::collections::hash_map::Entry;
use std::collections::HashMap;
use std::sync::Mutex;
use xcb::{x, Connection};

lazy_static! {
    static ref ATOM_CACHE: Mutex<HashMap<&'static str, x::Atom>> = Mutex::new(HashMap::new());
}

pub fn get(conn: &Connection, name: &'static str) -> Result<x::Atom> {
    let mut map = ATOM_CACHE
        .lock()
        .map_err(|_| anyhow!("Failed to access atom cache"))?;
    match map.entry(name) {
        Entry::Occupied(entry) => Ok(*entry.get()),
        Entry::Vacant(entry) => {
            let cookie = conn.send_request(&x::InternAtom {
                only_if_exists: true,
                name: name.as_bytes(),
            });
            let interned = conn.wait_for_reply(cookie)?.atom();
            Ok(*entry.insert(interned))
        }
    }
}
