// Copyright 2015-2019 Parity Technologies (UK) Ltd.
// This file is part of Parity Ethereum.

// Parity Ethereum is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.

// Parity Ethereum is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.

// You should have received a copy of the GNU General Public License
// along with Parity Ethereum.  If not, see <http://www.gnu.org/licenses/>.

//! Tokio runtime wrapper.
//!
//! Owns the multi-threaded tokio runtime that drives the RPC servers, the
//! pub-sub notification loops and the polling filters. Dropping the
//! [`Runtime`] shuts it down.

pub extern crate tokio;

use std::{fmt, future::Future};
pub use tokio::runtime::Handle;

/// An owned tokio runtime.
pub struct Runtime {
    runtime: Option<tokio::runtime::Runtime>,
    executor: Executor,
}

impl Runtime {
    fn new(builder: &mut tokio::runtime::Builder) -> Self {
        let runtime = builder
            .enable_all()
            .thread_name("rpc-runtime")
            .build()
            .expect(
                "Building a Tokio runtime will only fail when mio components \
                 cannot be initialized (catastrophic)",
            );
        let executor = Executor {
            handle: runtime.handle().clone(),
        };
        Runtime {
            runtime: Some(runtime),
            executor,
        }
    }

    /// Spawns a new tokio runtime with a default thread count (one worker
    /// per CPU core) and returns a `Runtime` which can be used to spawn
    /// tasks via its executor.
    pub fn with_default_thread_count() -> Self {
        Self::new(&mut tokio::runtime::Builder::new_multi_thread())
    }

    /// Spawns a new tokio runtime with the specified worker thread count.
    pub fn with_thread_count(thread_count: usize) -> Self {
        Self::new(
            tokio::runtime::Builder::new_multi_thread()
                .worker_threads(thread_count.max(1)),
        )
    }

    /// Returns the runtime executor.
    pub fn executor(&self) -> Executor { self.executor.clone() }

    /// Returns a handle to the underlying tokio runtime.
    pub fn handle(&self) -> &Handle { &self.executor.handle }
}

impl fmt::Debug for Runtime {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_struct("Runtime").finish()
    }
}

impl Drop for Runtime {
    fn drop(&mut self) {
        // `shutdown_background` does not block, so the runtime can also be
        // dropped from within an asynchronous context.
        if let Some(runtime) = self.runtime.take() {
            runtime.shutdown_background();
        }
    }
}

/// A cloneable handle used to spawn tasks on a [`Runtime`].
#[derive(Debug, Clone)]
pub struct Executor {
    handle: Handle,
}

impl Executor {
    /// Executor for an existing tokio runtime.
    pub fn new(handle: Handle) -> Self { Executor { handle } }

    /// Executor for the tokio runtime the caller currently runs in.
    ///
    /// Panics when called outside of a tokio runtime.
    pub fn current() -> Self {
        Executor {
            handle: Handle::current(),
        }
    }

    /// Spawn a future to this runtime.
    pub fn spawn<F>(&self, future: F)
    where F: Future<Output = ()> + Send + 'static {
        self.handle.spawn(future);
    }

    /// Returns a handle to the underlying tokio runtime.
    pub fn handle(&self) -> &Handle { &self.handle }
}

#[cfg(test)]
mod tests {
    use super::Runtime;
    use std::sync::mpsc;

    #[test]
    fn spawned_tasks_run() {
        let runtime = Runtime::with_thread_count(2);
        let (tx, rx) = mpsc::channel();
        runtime.executor().spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(1)).await;
            tx.send(42).unwrap();
        });
        assert_eq!(rx.recv().unwrap(), 42);
    }
}
