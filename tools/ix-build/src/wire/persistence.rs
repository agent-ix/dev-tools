// SPDX-License-Identifier: MIT
//! Scoped legacy JSON projection. Public status/diagnostics keep their current wire format.
use serde::{Serialize, Serializer};
use std::cell::Cell;
thread_local! { static LEGACY: Cell<bool> = const { Cell::new(false) }; }
struct Guard(bool);
impl Drop for Guard {
    fn drop(&mut self) {
        LEGACY.set(self.0);
    }
}
/// Whether this thread is serializing the legacy durable view.
pub fn active() -> bool {
    LEGACY.get()
}
/// Omit new strict-record fields only in the legacy durable view.
pub fn hide<T>(_: &T) -> bool {
    active()
}
/// Preserve schema 1 for the oldest supported deployed writer.
pub fn schema<S: Serializer>(value: &u32, serializer: S) -> Result<S::Ok, S::Error> {
    serializer.serialize_u32(if active() { 1 } else { *value })
}
/// Encode a legacy-readable durable view without changing concurrent public serialization.
pub fn to_vec<T: Serialize + ?Sized>(value: &T) -> serde_json::Result<Vec<u8>> {
    let _guard = Guard(LEGACY.replace(true));
    serde_json::to_vec(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wire::Code;

    /// Trace: FR-005-AC-4. Provenance: SWM-436.
    #[test]
    fn legacy_projection_covers_every_code_and_restores_public_wire() -> serde_json::Result<()> {
        for code in Code::ALL {
            let bytes = to_vec(&code)?;
            let spelling: String = serde_json::from_slice(&bytes)?;
            assert!(spelling.starts_with("LB_"));
            assert_eq!(
                serde_json::from_slice::<Code>(&bytes)?,
                code.legacy_equivalent()
            );
            assert_eq!(serde_json::to_value(code)?, code.as_str());
        }
        Ok(())
    }

    /// Trace: FR-005-AC-4. Provenance: SWM-436.
    #[test]
    fn failed_projection_restores_wire_and_is_thread_local() {
        struct Fails;
        impl Serialize for Fails {
            fn serialize<S: Serializer>(&self, _: S) -> Result<S::Ok, S::Error> {
                assert!(active());
                assert!(!std::thread::spawn(active).join().unwrap_or(true));
                Err(serde::ser::Error::custom("fixture error"))
            }
        }
        assert!(to_vec(&Fails).is_err());
        assert!(!active());
    }
}
