//! Pool de connexions en lecture seule.
//!
//! WAL autorise des lecteurs concurrents pendant qu'un écrivain travaille ; le pool
//! évite d'ouvrir une connexion par requête (coûteux : `PRAGMA`, mmap).

use crate::{Result, StoreError, open_connection};
use rusqlite::Connection;
use std::path::PathBuf;
use std::sync::{Arc, Condvar, Mutex};

#[derive(Clone)]
pub struct ReadPool {
    inner: Arc<PoolInner>,
}

struct PoolInner {
    path: PathBuf,
    free: Mutex<Vec<Connection>>,
    available: Condvar,
    capacity: usize,
}

impl ReadPool {
    pub fn new(path: &std::path::Path, capacity: usize) -> Result<ReadPool> {
        let capacity = capacity.max(1);
        let mut free = Vec::with_capacity(capacity);
        for _ in 0..capacity {
            free.push(open_connection(path, true)?);
        }
        Ok(ReadPool {
            inner: Arc::new(PoolInner {
                path: path.to_path_buf(),
                free: Mutex::new(free),
                available: Condvar::new(),
                capacity,
            }),
        })
    }

    pub fn capacity(&self) -> usize {
        self.inner.capacity
    }

    /// Emprunte une connexion, exécute `f`, rend la connexion au pool.
    pub fn with<T, F>(&self, f: F) -> Result<T>
    where
        F: FnOnce(&Connection) -> Result<T>,
    {
        let conn = self.acquire()?;
        let out = f(&conn);
        self.release(conn);
        out
    }

    fn acquire(&self) -> Result<Connection> {
        let mut guard = self
            .inner
            .free
            .lock()
            .map_err(|_| StoreError::other("pool de lecture empoisonné"))?;
        loop {
            if let Some(c) = guard.pop() {
                return Ok(c);
            }
            let (g, timeout) = self
                .inner
                .available
                .wait_timeout(guard, std::time::Duration::from_secs(5))
                .map_err(|_| StoreError::other("pool de lecture empoisonné"))?;
            guard = g;
            if timeout.timed_out() && guard.is_empty() {
                // Plutôt que de bloquer indéfiniment, on ouvre une connexion hors pool.
                return open_connection(&self.inner.path, true);
            }
        }
    }

    fn release(&self, conn: Connection) {
        if let Ok(mut guard) = self.inner.free.lock()
            && guard.len() < self.inner.capacity
        {
            guard.push(conn);
            self.inner.available.notify_one();
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::Store;

    #[test]
    fn pool_serves_more_readers_than_capacity() {
        let s = Store::open_memory().unwrap();
        s.write_blocking(|tx| crate::kv_set(tx, "x", "1")).unwrap();
        let mut hs = Vec::new();
        for _ in 0..16 {
            let s2 = s.clone();
            hs.push(std::thread::spawn(move || {
                for _ in 0..20 {
                    let v = s2.read_blocking(|c| crate::kv_get(c, "x")).unwrap();
                    assert_eq!(v.as_deref(), Some("1"));
                }
            }));
        }
        for h in hs {
            h.join().unwrap();
        }
    }
}
