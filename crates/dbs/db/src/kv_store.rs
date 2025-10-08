use kvdb::KeyValueDB;

/// Common abstraction over key/value backends used by the node.
pub trait KeyValueStore: KeyValueDB + Send + Sync {}

impl<T> KeyValueStore for T where T: KeyValueDB + Send + Sync {}

/// Convenience alias for trait objects storing key/value databases.
pub type DynKeyValueStore = dyn KeyValueStore;
