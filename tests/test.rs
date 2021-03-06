extern crate r2d2;

use std::sync::Mutex;
use std::default::Default;

mod config;

#[deriving(Show, PartialEq)]
struct FakeConnection;

#[deriving(Default)]
struct OkManager;

impl r2d2::PoolManager<FakeConnection, ()> for OkManager {
    fn connect(&self) -> Result<FakeConnection, ()> {
        Ok(FakeConnection)
    }

    fn is_valid(&self, _: &FakeConnection) -> bool {
        true
    }
}

struct NthConnectFailManager {
    n: Mutex<uint>,
}

impl r2d2::PoolManager<FakeConnection, ()> for NthConnectFailManager {
    fn connect(&self) -> Result<FakeConnection, ()> {
        let mut n = self.n.lock();
        if *n > 0 {
            *n -= 1;
            Ok(FakeConnection)
        } else {
            Err(())
        }
    }

    fn is_valid(&self, _: &FakeConnection) -> bool {
        true
    }
}

#[test]
fn test_initial_size_ok() {
    let config = r2d2::Config {
        initial_size: 5,
        ..Default::default()
    };
    let manager = NthConnectFailManager { n: Mutex::new(5) };
    assert!(r2d2::Pool::new(config, manager).is_ok());
}

#[test]
fn test_initial_size_err() {
    let config = r2d2::Config {
        initial_size: 5,
        ..Default::default()
    };
    let manager = NthConnectFailManager { n: Mutex::new(4) };
    assert_eq!(r2d2::Pool::new(config, manager).err().unwrap(), r2d2::ConnectionError(()));
}

#[test]
#[should_fail]
fn test_missing_replace() {
    fn hack_rust_14875(pool: &r2d2::Pool<FakeConnection, (), OkManager>) {
        pool.get().unwrap();
    }

    let pool = r2d2::Pool::new(Default::default(), OkManager).unwrap();
    hack_rust_14875(&pool);
}

#[test]
fn test_acquire_release() {
    let config = r2d2::Config {
        initial_size: 2,
        ..Default::default()
    };
    let pool = r2d2::Pool::new(config, OkManager).unwrap();

    let conn1 = pool.get().unwrap();
    let conn2 = pool.get().unwrap();
    conn1.replace();
    let conn3 = pool.get().unwrap();
    conn2.replace();
    conn3.replace();
}

#[test]
fn test_acquire_fail() {
    let config = r2d2::Config {
        initial_size: 0,
        ..Default::default()
    };
    let manager = NthConnectFailManager { n: Mutex::new(1) };
    let pool = r2d2::Pool::new(config, manager).unwrap();

    let c1 = pool.get().unwrap();
    assert!(pool.get().is_err());
    c1.replace();
}
