//! Per-host ledger: reservations held by leads (any host) and the queue of
//! tickets this host's leads are waiting on (lead host only).
//!
//! `~/.local/state/wrangle/` holds `lock/` (an atomic `create_dir` mutex),
//! `reservations.json`, and `queue.json`. Reservations expire by TTL so a lead
//! that dies between `admit` and `spawn` frees its slot without bookkeeping.

use std::fs;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::error::Result;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Reservation {
    pub ticket: String,
    pub lead: String,
    pub created_ms: u64,
    /// Set by `wrangle spawn` once the child's pane exists, so a `pane.closed`
    /// hook can release by pane.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pane: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QueueEntry {
    pub ticket: String,
    pub lead: String,
    pub created_ms: u64,
    /// Pinned machine, when the lead passed one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub machine: Option<String>,
}

#[derive(Debug, Clone)]
pub struct Ledger {
    dir: PathBuf,
}

/// How long a lock holder may keep the directory before another process
/// treats it as abandoned (a crashed `reserve`).
const LOCK_STALE: Duration = Duration::from_secs(10);
const LOCK_WAIT: Duration = Duration::from_secs(5);
const LOCK_STEP: Duration = Duration::from_millis(25);

#[must_use]
pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

/// `$XDG_STATE_HOME/wrangle`, else `~/.local/state/wrangle`.
#[must_use]
pub fn default_dir() -> PathBuf {
    if let Some(p) = std::env::var_os("WRANGLE_STATE") {
        return PathBuf::from(p);
    }
    let base = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local").join("state")))
        .unwrap_or_else(|| PathBuf::from(".local/state"));
    base.join("wrangle")
}

/// A fresh ticket id: `w-<unix ms, base 36>-<4 hex>`. Unique enough across
/// concurrent leads; the lock serialises writers on one host.
#[must_use]
pub fn new_ticket() -> String {
    let ms = now_ms();
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.subsec_nanos());
    let salt = (u64::from(std::process::id()) ^ u64::from(nanos)) & 0xffff;
    format!("w-{}-{salt:04x}", base36(ms))
}

fn base36(mut n: u64) -> String {
    const DIGITS: &[u8; 36] = b"0123456789abcdefghijklmnopqrstuvwxyz";
    if n == 0 {
        return "0".to_string();
    }
    let mut out = Vec::new();
    while n > 0 {
        out.push(DIGITS[(n % 36) as usize]);
        n /= 36;
    }
    out.reverse();
    String::from_utf8_lossy(&out).into_owned()
}

impl Ledger {
    #[must_use]
    pub fn new(dir: PathBuf) -> Self {
        Self { dir }
    }

    fn reservations_path(&self) -> PathBuf {
        self.dir.join("reservations.json")
    }

    fn queue_path(&self) -> PathBuf {
        self.dir.join("queue.json")
    }

    fn lock_path(&self) -> PathBuf {
        self.dir.join("lock")
    }

    /// Run `f` while holding the host lock. Waits up to `LOCK_WAIT`, breaking
    /// a lock older than `LOCK_STALE`.
    pub fn with_lock<T>(&self, f: impl FnOnce(&Self) -> Result<T>) -> Result<T> {
        fs::create_dir_all(&self.dir)?;
        let lock = self.lock_path();
        let deadline = std::time::Instant::now() + LOCK_WAIT;
        loop {
            match fs::create_dir(&lock) {
                Ok(()) => break,
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    if lock_is_stale(&lock) {
                        let _ = fs::remove_dir(&lock);
                        continue;
                    }
                    if std::time::Instant::now() >= deadline {
                        return Err(std::io::Error::new(
                            std::io::ErrorKind::WouldBlock,
                            format!("lock {} held for more than {LOCK_WAIT:?}", lock.display()),
                        )
                        .into());
                    }
                    thread::sleep(LOCK_STEP);
                }
                Err(e) => return Err(e.into()),
            }
        }
        let result = f(self);
        let _ = fs::remove_dir(&lock);
        result
    }

    fn write<T: Serialize>(&self, path: &Path, items: &[T]) -> Result<()> {
        fs::create_dir_all(&self.dir)?;
        let tmp = path.with_extension("json.tmp");
        fs::write(&tmp, serde_json::to_vec_pretty(items)?)?;
        fs::rename(&tmp, path)?;
        Ok(())
    }

    /// Reservations younger than `ttl_ms`. Call under the lock when the
    /// result decides anything.
    pub fn reservations(&self, ttl_ms: u64) -> Result<Vec<Reservation>> {
        let now = now_ms();
        let all: Vec<Reservation> = read_list(&self.reservations_path())?;
        Ok(all
            .into_iter()
            .filter(|r| now.saturating_sub(r.created_ms) < ttl_ms)
            .collect())
    }

    /// Drop expired reservations and append a new one. Under the lock.
    pub fn reserve(&self, reservation: Reservation, ttl_ms: u64) -> Result<()> {
        let mut live = self.reservations(ttl_ms)?;
        live.retain(|r| r.ticket != reservation.ticket);
        live.push(reservation);
        self.write(&self.reservations_path(), &live)
    }

    /// Remove reservations matching the predicate. Returns how many went.
    pub fn release(&self, ttl_ms: u64, matches: impl Fn(&Reservation) -> bool) -> Result<usize> {
        let live = self.reservations(ttl_ms)?;
        let before = live.len();
        let kept: Vec<Reservation> = live.into_iter().filter(|r| !matches(r)).collect();
        let removed = before - kept.len();
        if removed > 0 || before > 0 {
            self.write(&self.reservations_path(), &kept)?;
        }
        Ok(removed)
    }

    /// Attach a pane to a reservation so a `pane.closed` hook can release it.
    pub fn set_pane(&self, ticket: &str, pane: &str, ttl_ms: u64) -> Result<bool> {
        let mut live = self.reservations(ttl_ms)?;
        let mut found = false;
        for r in &mut live {
            if r.ticket == ticket {
                r.pane = Some(pane.to_string());
                found = true;
            }
        }
        if found {
            self.write(&self.reservations_path(), &live)?;
        }
        Ok(found)
    }

    pub fn queue(&self) -> Result<Vec<QueueEntry>> {
        read_list(&self.queue_path())
    }

    pub fn enqueue(&self, entry: QueueEntry) -> Result<()> {
        let mut q = self.queue()?;
        q.retain(|e| e.ticket != entry.ticket);
        q.push(entry);
        self.write(&self.queue_path(), &q)
    }

    pub fn dequeue(&self, ticket: &str) -> Result<Option<QueueEntry>> {
        let q = self.queue()?;
        let (hit, rest): (Vec<_>, Vec<_>) = q.into_iter().partition(|e| e.ticket == ticket);
        if hit.is_empty() {
            return Ok(None);
        }
        self.write(&self.queue_path(), &rest)?;
        Ok(hit.into_iter().next())
    }
}

/// A missing or empty file is an empty list.
fn read_list<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<Vec<T>> {
    match fs::read(path) {
        Ok(bytes) if bytes.is_empty() => Ok(Vec::new()),
        Ok(bytes) => Ok(serde_json::from_slice(&bytes)?),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(e.into()),
    }
}

fn lock_is_stale(lock: &Path) -> bool {
    fs::metadata(lock)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.elapsed().ok())
        .is_some_and(|age| age > LOCK_STALE)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn temp_ledger(name: &str) -> Ledger {
        let dir = std::env::temp_dir().join(format!("wrangle-test-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        Ledger::new(dir)
    }

    fn res(ticket: &str, created_ms: u64) -> Reservation {
        Reservation {
            ticket: ticket.to_string(),
            lead: "lead".to_string(),
            created_ms,
            pane: None,
        }
    }

    #[test]
    fn empty_ledger_reads_empty() {
        let l = temp_ledger("empty");
        assert_eq!(l.reservations(1000).unwrap().len(), 0);
        assert_eq!(l.queue().unwrap().len(), 0);
    }

    #[test]
    fn reserve_then_release_by_ticket() {
        let l = temp_ledger("reserve");
        l.reserve(res("t1", now_ms()), 60_000).unwrap();
        l.reserve(res("t2", now_ms()), 60_000).unwrap();
        assert_eq!(l.reservations(60_000).unwrap().len(), 2);
        assert_eq!(l.release(60_000, |r| r.ticket == "t1").unwrap(), 1);
        let left = l.reservations(60_000).unwrap();
        assert_eq!(left.len(), 1);
        assert_eq!(left[0].ticket, "t2");
    }

    #[test]
    fn expired_reservations_are_dropped_on_read() {
        let l = temp_ledger("ttl");
        l.reserve(res("old", now_ms() - 10_000), 60_000).unwrap();
        l.reserve(res("new", now_ms()), 60_000).unwrap();
        let live = l.reservations(5_000).unwrap();
        assert_eq!(live.len(), 1);
        assert_eq!(live[0].ticket, "new");
    }

    #[test]
    fn set_pane_then_release_by_pane() {
        let l = temp_ledger("pane");
        l.reserve(res("t1", now_ms()), 60_000).unwrap();
        assert!(l.set_pane("t1", "w1:p4", 60_000).unwrap());
        assert!(!l.set_pane("nope", "w1:p4", 60_000).unwrap());
        assert_eq!(
            l.release(60_000, |r| r.pane.as_deref() == Some("w1:p4"))
                .unwrap(),
            1
        );
    }

    #[test]
    fn queue_round_trip() {
        let l = temp_ledger("queue");
        l.enqueue(QueueEntry {
            ticket: "q1".into(),
            lead: "lead".into(),
            created_ms: 1,
            machine: None,
        })
        .unwrap();
        assert_eq!(l.queue().unwrap().len(), 1);
        assert!(l.dequeue("missing").unwrap().is_none());
        assert_eq!(l.dequeue("q1").unwrap().unwrap().ticket, "q1");
        assert_eq!(l.queue().unwrap().len(), 0);
    }

    #[test]
    fn lock_serialises_concurrent_reservers() {
        let l = temp_ledger("lock");
        let counter = AtomicUsize::new(0);
        thread::scope(|s| {
            for i in 0..8 {
                let l = &l;
                let counter = &counter;
                s.spawn(move || {
                    l.with_lock(|l| {
                        // Read-modify-write without the lock would lose updates.
                        let live = l.reservations(60_000)?;
                        thread::sleep(Duration::from_millis(5));
                        l.reserve(res(&format!("t{i}"), now_ms()), 60_000)?;
                        counter.fetch_add(1, Ordering::SeqCst);
                        assert_eq!(l.reservations(60_000)?.len(), live.len() + 1);
                        Ok(())
                    })
                    .unwrap();
                });
            }
        });
        assert_eq!(counter.load(Ordering::SeqCst), 8);
        assert_eq!(l.reservations(60_000).unwrap().len(), 8);
        assert!(!l.lock_path().exists());
    }

    #[test]
    fn stale_lock_is_broken() {
        let l = temp_ledger("stale");
        fs::create_dir_all(&l.dir).unwrap();
        fs::create_dir(l.lock_path()).unwrap();
        let old = SystemTime::now() - LOCK_STALE - Duration::from_secs(5);
        fs::File::open(l.lock_path())
            .unwrap()
            .set_modified(old)
            .unwrap();
        l.with_lock(|_| Ok(())).unwrap();
        assert!(!l.lock_path().exists());
    }

    #[test]
    fn tickets_are_distinct() {
        let a = new_ticket();
        let b = new_ticket();
        assert!(a.starts_with("w-"));
        assert_ne!(a, b);
    }
}
