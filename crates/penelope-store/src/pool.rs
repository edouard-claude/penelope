//! Pool de connexions en lecture seule.
//!
//! WAL autorise des lecteurs concurrents pendant qu'un écrivain travaille ; le pool
//! évite d'ouvrir une connexion par requête (coûteux : `PRAGMA`, mmap).

use crate::{Result, StoreError, open_connection};
use rusqlite::Connection;
use std::path::PathBuf;
use std::sync::{Arc, Condvar, Mutex};
use std::time::Instant;

/// Ce qu'on sait d'une connexion du pool (issue #158) : une connexion ouverte depuis des
/// heures a rendu « malformed inverted index » sur une base que toute connexion neuve
/// relisait intègre. Son âge et le nombre de requêtes servies sont la première piste.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReaderStats {
    pub age_s: u64,
    pub served: u64,
}

/// Une connexion et son histoire.
struct Reader {
    conn: Connection,
    opened_at: Instant,
    served: u64,
}

#[derive(Clone)]
pub struct ReadPool {
    inner: Arc<PoolInner>,
}

struct PoolInner {
    path: PathBuf,
    free: Mutex<Vec<Reader>>,
    available: Condvar,
    capacity: usize,
}

impl ReadPool {
    pub fn new(path: &std::path::Path, capacity: usize) -> Result<ReadPool> {
        let capacity = capacity.max(1);
        let mut free = Vec::with_capacity(capacity);
        for _ in 0..capacity {
            free.push(Reader {
                conn: open_connection(path, true)?,
                opened_at: Instant::now(),
                served: 0,
            });
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
        let mut reader = self.acquire()?;
        let out = f(&reader.conn);
        reader.served += 1;
        self.release(reader);
        out
    }

    /// Comme [`with`](Self::with), mais `f` dit si la connexion mérite de rester dans le
    /// pool, et l'appelant reçoit son âge et ce qu'elle a servi (issue #158).
    ///
    /// Une connexion qui rend un verdict que le fichier dément n'a plus rien à faire là :
    /// elle est fermée, et le pool en rouvre une à sa place au prochain emprunt.
    pub fn with_audit<T, F>(&self, f: F) -> Result<(T, ReaderStats)>
    where
        F: FnOnce(&Connection) -> Result<(T, bool)>,
    {
        let mut reader = self.acquire()?;
        let stats = ReaderStats {
            age_s: reader.opened_at.elapsed().as_secs(),
            served: reader.served,
        };
        let out = f(&reader.conn);
        reader.served += 1;
        match &out {
            Ok((_, false)) => drop(reader),
            _ => self.release(reader),
        }
        out.map(|(v, _)| (v, stats))
    }

    fn acquire(&self) -> Result<Reader> {
        let mut guard = self
            .inner
            .free
            .lock()
            .map_err(|_| StoreError::other("pool de lecture empoisonné"))?;
        loop {
            if let Some(r) = guard.pop() {
                return Ok(r);
            }
            let (g, timeout) = self
                .inner
                .available
                .wait_timeout(guard, std::time::Duration::from_secs(5))
                .map_err(|_| StoreError::other("pool de lecture empoisonné"))?;
            guard = g;
            if timeout.timed_out() && guard.is_empty() {
                // Plutôt que de bloquer indéfiniment, on ouvre une connexion hors pool.
                return Ok(Reader {
                    conn: open_connection(&self.inner.path, true)?,
                    opened_at: Instant::now(),
                    served: 0,
                });
            }
        }
    }

    fn release(&self, conn: Reader) {
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
