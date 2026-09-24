//! Test helpers shared across crates (dev-dependency only). Fixtures, builders and test-tenant
//! helpers land here as the modules that need them arrive (WP1+).

use std::sync::Arc;

use object_store::memory::InMemory;
use platform::storage::Storage;

/// Object storage that is always reachable and starts empty.
pub fn memory_storage() -> Storage {
    Storage {
        public: Arc::new(InMemory::new()),
        private: Arc::new(InMemory::new()),
    }
}
