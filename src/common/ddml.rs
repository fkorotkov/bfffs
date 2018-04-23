// vim: tw=80
///! Direct Data Management Layer
///
/// Interface for working with Direct Records.  Unifies cache, compression,
/// disk, and hash operations.  A Direct Record is a record that can never be
/// duplicated, either through snapshots, clones, or deduplication.

use common::*;
use common::cache::*;
use common::pool::*;
use futures::{Future, future};
use metrohash::MetroHash64;
use nix::{Error, errno};
use std::hash::Hasher;
use std::sync::Mutex;
#[cfg(test)]
use uuid::Uuid;

#[cfg(test)]
/// Only exists so mockers can replace Cache
pub trait CacheTrait {
    fn get(&mut self, key: &Key) -> Option<DivBuf>;
    fn insert(&mut self, key: Key, buf: DivBufShared);
    fn remove(&mut self, key: &Key) -> Option<DivBufShared>;
    fn size(&self) -> usize;
}
#[cfg(test)]
pub type CacheLike = Box<CacheTrait>;
#[cfg(not(test))]
#[doc(hidden)]
pub type CacheLike = Cache;

#[cfg(test)]
/// Only exists so mockers can replace Pool
pub trait PoolTrait {
    fn free(&self, pba: PBA, length: LbaT);
    fn name(&self) -> &str;
    fn read(&self, buf: IoVecMut, pba: PBA) -> Box<PoolFut>;
    fn sync_all(&self) -> Box<PoolFut>;
    fn uuid(&self) -> Uuid;
    fn write(&self, buf: IoVec) -> Result<(PBA, Box<PoolFut>), Error>;
    fn write_label(&self) -> Box<PoolFut>;
}
#[cfg(test)]
pub type PoolLike = Box<PoolTrait>;
#[cfg(not(test))]
#[doc(hidden)]
pub type PoolLike = Pool;

/// Default cache size.
#[cfg(not(test))]
const CACHE_SIZE: usize = 1_000_000_000;

/// Compression mode in use
#[derive(Clone, Copy, Debug)]
pub enum Compression {
    None,
    // TODO: add more types
}

/// Direct Record Pointer.  A persistable pointer to a record on disk.
///
/// A Record is a local unit of data on disk.  It may be larger or smaller than
/// a Block, but Records are always read/written in their entirety.
#[derive(Clone, Copy, Debug)]
pub struct DRP {
    /// Physical Block Address.  The record's location on disk.
    pba: PBA,
    /// Compression algorithm in use
    _compression: Compression,
    /// Logical size.  Uncompressed size of the record
    lsize: u32,
    /// Compressed size.
    csize: u32,
    /// Checksum of the compressed record.
    checksum: u64
}

impl DRP {
    /// Return the storage space actually allocated for this record
    fn asize(&self) -> LbaT {
        div_roundup(self.csize as usize, BYTES_PER_LBA) as LbaT
    }
}

/// Direct Data Management Layer for a single `Pool`
pub struct DDML {
    // Sadly, the Cache needs to be Mutex-protected because updating the LRU
    // list requires exclusive access.  It can be a normal Mutex instead of a
    // futures_lock::Mutex, because we will never need to block while holding
    // this lock.
    cache: Mutex<CacheLike>,
    pool: PoolLike,
}

impl<'a> DDML {
    /// Initialze the `DDML`.
    #[cfg(not(test))]
    pub fn create(pool: PoolLike) -> Self {
        DDML::new(pool, Cache::with_capacity(CACHE_SIZE))
    }

    /// Delete the record from the cache, and free its storage space.
    pub fn delete(&self, drp: &DRP) {
        self.cache.lock().unwrap().remove(&Key::PBA(drp.pba));
        self.pool.free(drp.pba, drp.asize());
    }

    /// If the given record is present in the cache, evict it.
    pub fn evict(&self, drp: &DRP) {
        self.cache.lock().unwrap().remove(&Key::PBA(drp.pba));
    }

    #[cfg(any(not(test), feature = "mocks"))]
    fn new(pool: PoolLike, cache: CacheLike) -> Self {
        DDML{pool: pool, cache: Mutex::new(cache)}
    }

    /// Read a record and return a shared reference
    pub fn get(&'a self, drp: &DRP)
        -> Box<Future<Item=DivBuf, Error=Error> + 'a> {

        // Outline:
        // 1) Fetch from cache, or
        let pba = drp.pba;
        self.cache.lock().unwrap().get(&Key::PBA(pba)).map(|db| {
            let r : Box<Future<Item=DivBuf, Error=Error>> =
            Box::new(future::ok::<DivBuf, Error>(db));
            r
        }).unwrap_or_else(|| {
            Box::new(
                self.read(*drp).map(move |dbs| {
                    let db = dbs.try().unwrap();
                    self.cache.lock().unwrap().insert(Key::PBA(pba), dbs);
                    db
                })
            )
        })
    }

    /// Read a record and return ownership of it.
    pub fn pop(&'a self, drp: &DRP)
        -> Box<Future<Item=DivBufShared, Error=Error> + 'a> {

        let lbas = drp.asize();
        let pba = drp.pba;
        self.cache.lock().unwrap().remove(&Key::PBA(pba)).map(|dbs| {
            self.pool.free(pba, lbas);
            let r : Box<Future<Item=DivBufShared, Error=Error>> =
            Box::new(future::ok::<DivBufShared, Error>(dbs));
            r
        }).unwrap_or_else(|| {
            Box::new(
                self.read(*drp).map(move |dbs| {
                    self.pool.free(pba, lbas);
                    dbs
                })
            )
        })
    }

    /// Write a record to disk and cache.  Return its Direct Record Pointer
    pub fn put(&'a self, buf: DivBufShared, _compression: Compression)
        -> (DRP, Box<Future<Item=(), Error=Error> + 'a>) {
        // Outline:
        // 1) Cache
        // 2) Compress
        // 3) Hash
        // 4) Pad
        // 5) Write

        let db = buf.try().unwrap();
        assert!(db.len() < u32::max_value() as usize,
            "Record exceeds maximum allowable length");
        let lsize = db.len() as u32;
        // TODO: compress buffer
        let csize = lsize;
        let mut hasher = MetroHash64::new();
        checksum_iovec(&db, &mut hasher);
        let checksum = hasher.finish();
        let (pba, fut) = self.pool.write(db).unwrap();
        self.cache.lock().unwrap().insert(Key::PBA(pba), buf);
        let drp = DRP {
            pba,
            _compression,
            lsize,
            csize,
            checksum
        };
        (drp, fut)
    }

    /// Read a record from disk
    fn read(&'a self, drp: DRP)
        -> Box<Future<Item=DivBufShared, Error=Error> + 'a> {

        // Outline
        // 1) Read
        // 2) Truncate
        // 3) Verify checksum
        // 4) Decompress
        let len = drp.asize() as usize * BYTES_PER_LBA;
        let dbs = DivBufShared::from(vec![0u8; len]);
        Box::new(
            self.pool.read(dbs.try_mut().unwrap(), drp.pba).and_then(move |_| {
                let mut dbm = dbs.try_mut().unwrap();
                // TODO: test short records
                dbm.try_truncate(drp.csize as usize).unwrap();
                let db = dbm.freeze();
                let mut hasher = MetroHash64::new();
                checksum_iovec(&db, &mut hasher);
                let checksum = hasher.finish();
                if checksum == drp.checksum {
                    Ok(dbs)
                } else {
                    // TODO: create a dedicated ECKSUM error type
                    Err(Error::Sys(errno::Errno::EIO))
                }
            })
        )
    }
}

#[cfg(test)]
#[cfg(feature = "mocks")]
mod t {

    use super::*;
    use divbuf::DivBufShared;
    use futures::future;
    use mockers::matchers::ANY;
    use mockers::{Scenario, Sequence};
    use std::cell::RefCell;
    use std::rc::Rc;
    use tokio::executor::current_thread;

    mock!{
        MockCache,
        self,
        trait CacheTrait {
            fn get(&mut self, key: &Key) -> Option<DivBuf>;
            fn insert(&mut self, key: Key, buf: DivBufShared);
            fn remove(&mut self, key: &Key) -> Option<DivBufShared>;
            fn size(&self) -> usize;
        }
    }

    mock!{
        MockPool,
        self,
        trait PoolTrait {
            fn free(&self, pba: PBA, length: LbaT);
            fn name(&self) -> &str;
            fn read(&self, buf: IoVecMut, pba: PBA) -> Box<PoolFut<'static>>;
            fn sync_all(&self) -> Box<PoolFut>;
            fn uuid(&self) -> Uuid;
            fn write(&self, buf: IoVec)
                -> Result<(PBA, Box<PoolFut<'static>>), Error>;
            fn write_label(&self) -> Box<PoolFut>;
        }
    }

    #[test]
    fn delete_hot() {
        let pba = PBA::new(7, 42);
        let drp = DRP{pba, _compression: Compression::None, lsize: 4096,
                      csize: 4096, checksum: 0};
        let pba2 = pba.clone();
        let dbs = DivBufShared::from(vec![0u8; 4096]);
        let s = Scenario::new();
        let mut seq = Sequence::new();
        let cache = s.create_mock::<MockCache>();
        let pool = s.create_mock::<MockPool>();
        seq.expect(cache.remove_call(check!(move |key: &&Key| {
            **key == Key::PBA(pba2)
        })).and_return(Some(dbs)));
        seq.expect(pool.free_call(pba, 1).and_return(()));
        s.expect(seq);

        let ddml = DDML::new(Box::new(pool), Box::new(cache));
        ddml.delete(&drp);
    }

    #[test]
    fn get_hot() {
        let pba = PBA::new(7, 42);
        let drp = DRP{pba, _compression: Compression::None, lsize: 4096,
                      csize: 4096, checksum: 0};
        let pba2 = pba.clone();
        let dbs = DivBufShared::from(vec![0u8; 4096]);
        let s = Scenario::new();
        let mut seq = Sequence::new();
        let cache = s.create_mock::<MockCache>();
        let pool = s.create_mock::<MockPool>();
        seq.expect(cache.get_call(check!(move |key: &&Key| {
            **key == Key::PBA(pba2)
        })).and_return(Some(dbs.try().unwrap())));
        s.expect(seq);

        let ddml = DDML::new(Box::new(pool), Box::new(cache));
        ddml.get(&drp);
    }

    #[test]
    fn get_cold() {
        let pba = PBA::new(7, 42);
        let drp = DRP{pba, _compression: Compression::None, lsize: 4096,
                      csize: 1, checksum: 0xe7f15966a3d61f8};
        let pba2 = pba.clone();
        let owned_by_cache = Rc::new(RefCell::new(Vec::<DivBufShared>::new()));
        let owned_by_cache2 = owned_by_cache.clone();
        let s = Scenario::new();
        let mut seq = Sequence::new();
        let cache = s.create_mock::<MockCache>();
        let pool = s.create_mock::<MockPool>();
        seq.expect(cache.get_call(check!(move |key: &&Key| {
            **key == Key::PBA(pba2)
        })).and_return(None));
        seq.expect(pool.read_call(check!(|dbm: &DivBufMut| {
            dbm.len() == 4096
        }), pba).and_return(Box::new(future::ok::<(), Error>(()))));
        seq.expect(cache.insert_call(Key::PBA(pba), ANY)
                   .and_call(move |_, dbs| {
                       owned_by_cache2.borrow_mut().push(dbs);
                   }));
        s.expect(seq);

        let ddml = DDML::new(Box::new(pool), Box::new(cache));
        current_thread::block_on_all(future::lazy(|| {
            ddml.get(&drp)
        })).unwrap();
    }

    #[test]
    fn get_ecksum() {
        let pba = PBA::new(7, 42);
        let drp = DRP{pba, _compression: Compression::None, lsize: 4096,
                      csize: 1, checksum: 0xdeadbeefdeadbeef};
        let pba2 = pba.clone();
        let s = Scenario::new();
        let mut seq = Sequence::new();
        let cache = s.create_mock::<MockCache>();
        let pool = s.create_mock::<MockPool>();
        seq.expect(cache.get_call(check!(move |key: &&Key| {
            **key == Key::PBA(pba2)
        })).and_return(None));
        seq.expect(pool.read_call(check!(|dbm: &DivBufMut| {
            dbm.len() == 4096
        }), pba).and_return(Box::new(future::ok::<(), Error>(()))));
        s.expect(seq);

        let ddml = DDML::new(Box::new(pool), Box::new(cache));
        let err = current_thread::block_on_all(future::lazy(|| {
            ddml.get(&drp)
        })).unwrap_err();
        assert_eq!(err, Error::Sys(errno::Errno::EIO));
    }

    #[test]
    fn pop_hot() {
        let pba = PBA::new(7, 42);
        let drp = DRP{pba, _compression: Compression::None, lsize: 4096,
                      csize: 4096, checksum: 0};
        let pba2 = pba.clone();
        let s = Scenario::new();
        let mut seq = Sequence::new();
        let cache = s.create_mock::<MockCache>();
        let pool = s.create_mock::<MockPool>();
        seq.expect(cache.remove_call(check!(move |key: &&Key| {
            **key == Key::PBA(pba2)
        })).and_return(Some(DivBufShared::from(vec![0u8; 4096]))));
        seq.expect(pool.free_call(pba, 1).and_return(()));
        s.expect(seq);

        let ddml = DDML::new(Box::new(pool), Box::new(cache));
        ddml.pop(&drp);
    }

    #[test]
    fn pop_cold() {
        let pba = PBA::new(7, 42);
        let pba2 = pba.clone();
        let drp = DRP{pba, _compression: Compression::None, lsize: 4096,
                      csize: 1, checksum: 0xe7f15966a3d61f8};
        let s = Scenario::new();
        let mut seq = Sequence::new();
        let cache = s.create_mock::<MockCache>();
        let pool = s.create_mock::<MockPool>();
        seq.expect(cache.remove_call(check!(move |key: &&Key| {
            **key == Key::PBA(pba2)
        })).and_return(None));
        seq.expect(pool.read_call(ANY, pba)
                   .and_return(Box::new(future::ok::<(), Error>(()))));
        seq.expect(pool.free_call(pba, 1).and_return(()));
        s.expect(seq);

        let ddml = DDML::new(Box::new(pool), Box::new(cache));
        current_thread::block_on_all(future::lazy(|| {
            ddml.pop(&drp)
        })).unwrap();
    }

    #[test]
    fn pop_ecksum() {
        let pba = PBA::new(7, 42);
        let pba2 = pba.clone();
        let drp = DRP{pba, _compression: Compression::None, lsize: 4096,
                      csize: 1, checksum: 0xdeadbeefdeadbeef};
        let s = Scenario::new();
        let mut seq = Sequence::new();
        let cache = s.create_mock::<MockCache>();
        let pool = s.create_mock::<MockPool>();
        seq.expect(cache.remove_call(check!(move |key: &&Key| {
            **key == Key::PBA(pba2)
        })).and_return(None));
        seq.expect(pool.read_call(ANY, pba)
                   .and_return(Box::new(future::ok::<(), Error>(()))));
        s.expect(seq);

        let ddml = DDML::new(Box::new(pool), Box::new(cache));
        let err = current_thread::block_on_all(future::lazy(|| {
            ddml.pop(&drp)
        })).unwrap_err();
        assert_eq!(err, Error::Sys(errno::Errno::EIO));
    }

    #[test]
    fn put() {
        let s = Scenario::new();
        let cache = s.create_mock::<MockCache>();
        let pba = PBA::new(7, 42);
        s.expect(cache.insert_call(Key::PBA(pba), ANY).and_return(()));
        let pool = s.create_mock::<MockPool>();
        s.expect(pool.write_call(ANY)
            .and_return(Ok((pba, Box::new(future::ok::<(), Error>(())))))
        );

        let ddml = DDML::new(Box::new(pool), Box::new(cache));
        let dbs = DivBufShared::from(vec![42u8; 4096]);
        let (drp, _) = ddml.put(dbs, Compression::None);
        assert_eq!(drp.pba, pba);
        assert_eq!(drp.csize, 4096);
        assert_eq!(drp.lsize, 4096);
    }
}
