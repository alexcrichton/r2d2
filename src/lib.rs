//! A library providing a generic connection pool.
#![feature(unsafe_destructor)]
#![warn(missing_doc)]
#![doc(html_root_url="http://www.rust-ci.org/sfackler/r2d2/doc")]

use std::comm;
use std::cmp;
use std::collections::{Deque, RingBuf};
use std::sync::{Arc, Mutex};
use std::fmt;

pub use config::Config;

mod config;

/// A trait which provides database-specific functionality.
pub trait PoolManager<C, E>: Send+Sync {
    /// Attempts to create a new connection.
    fn connect(&self) -> Result<C, E>;

    /// Determines if the connection is still connected to the database.
    ///
    /// A standard implementation would check if a simple query like `SELECT 1`
    /// succeeds.
    fn is_valid(&self, conn: &C) -> bool;
}

/// An error type returned if pool creation fails.
#[deriving(PartialEq, Eq)]
pub enum NewPoolError<E> {
    /// The provided pool configuration was invalid.
    InvalidConfig(&'static str),
    /// The manager returned an error when creating a connection.
    ConnectionError(E),
}

impl<E: fmt::Show> fmt::Show for NewPoolError<E> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match *self {
            InvalidConfig(ref error) => write!(f, "Invalid config: {}", error),
            ConnectionError(ref error) => write!(f, "Unable to create connections: {}", error),
        }
    }
}

enum Command<C> {
    AddConnection,
    TestConnection(C),
}

struct PoolInternals<C, E> {
    conns: RingBuf<C>,
    failed_conns: RingBuf<E>,
    num_conns: uint,
}

struct InnerPool<C, E, M> {
    config: Config,
    manager: M,
    internals: Mutex<PoolInternals<C, E>>,
}

/// A generic connection pool.
pub struct Pool<C, E, M> {
    helper_chan: Sender<Command<C>>,
    inner: Arc<InnerPool<C, E, M>>
}

impl<C: Send, E: Send, M: PoolManager<C, E>> Pool<C, E, M> {
    /// Creates a new connection pool.
    pub fn new(config: Config, manager: M) -> Result<Pool<C, E, M>, NewPoolError<E>> {
        match config.validate() {
            Ok(()) => {}
            Err(err) => return Err(InvalidConfig(err))
        }

        let mut internals = PoolInternals {
            conns: RingBuf::new(),
            failed_conns: RingBuf::new(),
            num_conns: config.initial_size,
        };

        for _ in range(0, config.initial_size) {
            match manager.connect() {
                Ok(conn) => internals.conns.push(conn),
                Err(err) => return Err(ConnectionError(err)),
            }
        }

        let inner = Arc::new(InnerPool {
            config: config,
            manager: manager,
            internals: Mutex::new(internals),
        });

        let (sender, receiver) = comm::channel();
        // FIXME :(
        let receiver = Arc::new(Mutex::new(receiver));

        for _ in range(0, config.helper_tasks) {
            let inner = inner.clone();
            let receiver = receiver.clone();
            spawn(proc() helper_task(receiver, inner));
        }

        Ok(Pool {
            helper_chan: sender,
            inner: inner,
        })
    }

    /// Retrieves a connection from the pool.
    pub fn get<'a>(&'a self) -> Result<PooledConnection<'a, C, E, M>, E> {
        let mut internals = self.inner.internals.lock();

        loop {
            match internals.conns.pop_front() {
                Some(conn) => {
                    if self.inner.config.test_on_check_out && !self.inner.manager.is_valid(&conn) {
                        internals.num_conns -= 1;
                        continue;
                    }

                    return Ok(PooledConnection {
                        pool: self,
                        conn: Some(conn)
                    })
                }
                None => {
                    match internals.failed_conns.pop_front() {
                        Some(err) => return Err(err),
                        None => {}
                    }

                    let new_conns = cmp::min(self.inner.config.max_size - internals.num_conns,
                                             self.inner.config.acquire_increment);
                    for _ in range(0, new_conns) {
                        self.helper_chan.send(AddConnection);
                        internals.num_conns += 1;
                    }

                    internals.cond.wait();
                }
            }
        }
    }

    fn put_back(&self, conn: C) {
        let mut internals = self.inner.internals.lock();
        internals.conns.push(conn);
        internals.cond.signal();
    }
}

fn helper_task<C: Send, E: Send, M: PoolManager<C, E>>(receiver: Arc<Mutex<Receiver<Command<C>>>>,
                                                       inner: Arc<InnerPool<C, E, M>>) {
    loop {
        let mut receiver = receiver.lock();
        let res = receiver.recv_opt();
        drop(receiver);

        match res {
            Ok(AddConnection) => add_connection(&*inner),
            Ok(TestConnection(conn)) => test_connection(&*inner, conn),
            Err(()) => break,
        }
    }
}

fn add_connection<C: Send, E: Send, M: PoolManager<C, E>>(inner: &InnerPool<C, E, M>) {
    let res = inner.manager.connect();
    let mut internals = inner.internals.lock();
    match res {
        Ok(conn) => {
            internals.conns.push(conn);
        }
        Err(err) => {
            internals.failed_conns.push(err);
            internals.num_conns -= 1;
        }
    }
    internals.cond.signal();
}

fn test_connection<C: Send, E: Send, M: PoolManager<C, E>>(inner: &InnerPool<C, E, M>, conn: C) {
    let is_valid = inner.manager.is_valid(&conn);
    let mut internals = inner.internals.lock();
    if is_valid {
        internals.conns.push(conn);
    } else {
        internals.num_conns -= 1;
    }
}

/// A smart pointer wrapping an underlying connection.
///
/// ## Note
///
/// Due to Rust bug [#15905](https://github.com/rust-lang/rust/issues/15905),
/// the connection cannot be automatically returned to its pool when the
/// `PooledConnection` drops out of scope. The `replace` method must be called,
/// or the `PooledConnection`'s destructor will `fail!()`.
pub struct PooledConnection<'a, C, E, M> {
    pool: &'a Pool<C, E, M>,
    conn: Option<C>,
}

impl<'a, C: Send, E: Send, M: PoolManager<C, E>> PooledConnection<'a, C, E, M> {
    /// Consumes the `PooledConnection`, returning the connection to its pool.
    ///
    /// This must be called before the `PooledConnection` drops out of scope or
    /// its destructor will `fail!()`.
    pub fn replace(mut self) {
        self.pool.put_back(self.conn.take_unwrap())
    }
}

#[unsafe_destructor]
impl<'a, C, E, M> Drop for PooledConnection<'a, C, E, M> {
    fn drop(&mut self) {
        if self.conn.is_some() {
            fail!("You must call conn.replace()");
        }
    }
}

impl<'a, C, E, M> Deref<C> for PooledConnection<'a, C, E, M> {
    fn deref(&self) -> &C {
        self.conn.get_ref()
    }
}
