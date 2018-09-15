// vim: tw=80
// LCOV_EXCL_START
use common::{
    *,
    dml::Compression,
    tree::{Key, Value},
};
use futures::{
    Future,
    Stream,
};
use simulacrum::*;
use std::{
    borrow::Borrow,
    marker::PhantomData,
    ops::RangeBounds
};

pub struct ReadOnlyDatasetMock<K: Key, V: Value> {
    e: Expectations,
    a: PhantomData<K>,
    b: PhantomData<V>,
}

impl<K: Key, V: Value> ReadOnlyDatasetMock<K, V> {
    pub fn allocated(&self) -> LbaT {
        self.e.was_called_returning::<(), LbaT> ("allocated", ())
    }

    pub fn expect_allocated(&mut self) -> Method<(), LbaT> {
        self.e.expect::<(), LbaT>("allocated")
    }

    pub fn get(&self, k: K) -> impl Future<Item=Option<V>, Error=Error>
    {
        self.e.was_called_returning::<K,
            Box<Future<Item=Option<V>, Error=Error> + Send>>("get", k)
    }

    pub fn expect_get(&mut self) -> Method<K,
        Box<Future<Item=Option<V>, Error=Error> + Send>>
    {
        self.e.expect::<K, Box<Future<Item=Option<V>, Error=Error> + Send>>
            ("get")
    }

    pub fn get_blob(&self, ridp: &RID)
        -> impl Future<Item=Box<DivBuf>, Error=Error> + Send
    {
        self.e.was_called_returning::<*const RID,
            Box<Future<Item=Box<DivBuf>, Error=Error> + Send>>
            ("get_blob", ridp as *const RID)
    }

    pub fn expect_get_blob(&mut self) -> Method<*const RID,
        Box<Future<Item=Box<DivBuf>, Error=Error> + Send>>
    {
        self.e.expect::<*const RID,
            Box<Future<Item=Box<DivBuf>, Error=Error> + Send>>("get_blob")
    }

    pub fn range<R, T>(&self, range: R) -> impl Stream<Item=(K, V), Error=Error>
        where K: Borrow<T>,
              R: RangeBounds<T> + 'static,
              T: Ord + Clone + Send + 'static
    {
        self.e.was_called_returning::<R,
            Box<Stream<Item=(K, V), Error=Error> + Send>>
            ("range", range)
    }

    pub fn expect_range<R, T>(&mut self)
        -> Method<R, Box<Stream<Item=(K, V), Error=Error> + Send>>
        where K: Borrow<T>,
              R: RangeBounds<T> + 'static,
              T: Ord + Clone + Send + 'static
    {
        self.e.expect::<R, Box<Stream<Item=(K, V), Error=Error> + Send>>
            ("range")
    }

    pub fn last_key(&self) -> impl Future<Item=Option<K>, Error=Error>
    {
        self.e.was_called_returning::<(),
            Box<Future<Item=Option<K>, Error=Error> + Send>>("last_key", ())
    }

    pub fn expect_last_key(&mut self) -> Method<(),
        Box<Future<Item=Option<K>, Error=Error> + Send>>
    {
        self.e.expect::<(), Box<Future<Item=Option<K>, Error=Error> + Send>>
            ("last_key")
    }

    pub fn new() -> Self {
        Self {
            e: Expectations::new(),
            a: PhantomData,
            b: PhantomData,
        }
    }

    pub fn size(&self) -> LbaT {
        self.e.was_called_returning::<(), LbaT> ("size", ())
    }

    pub fn expect_size(&mut self) -> Method<(), LbaT> {
        self.e.expect::<(), LbaT>("size")
    }

    pub fn then(&mut self) -> &mut Self {
        self.e.then();
        self
    }
}

pub struct ReadWriteDatasetMock<K: Key, V: Value> {
    e: Expectations,
    a: PhantomData<K>,
    b: PhantomData<V>,
}

impl<K: Key, V: Value> ReadWriteDatasetMock<K, V> {
    pub fn allocated(&self) -> LbaT {
        self.e.was_called_returning::<(), LbaT> ("allocated", ())
    }

    pub fn expect_allocated(&mut self) -> Method<(), LbaT> {
        self.e.expect::<(), LbaT>("allocated")
    }

    pub fn delete_blob(&self, ridp: &RID) -> impl Future<Item=(), Error=Error> {
        self.e.was_called_returning::<*const RID,
            Box<Future<Item=(), Error=Error> + Send>>
            ("delete_blob", ridp as *const RID)
    }

    pub fn expect_delete_blob(&mut self) -> Method<*const RID,
        Box<Future<Item=(), Error=Error> + Send>>
    {
        self.e.expect::<*const RID, Box<Future<Item=(), Error=Error> + Send>>
            ("delete_blob")
    }

    pub fn get(&self, k: K) -> impl Future<Item=Option<V>, Error=Error>
    {
        self.e.was_called_returning::<K,
            Box<Future<Item=Option<V>, Error=Error> + Send>>("get", k)
    }

    pub fn expect_get(&mut self) -> Method<K,
        Box<Future<Item=Option<V>, Error=Error> + Send>>
    {
        self.e.expect::<K, Box<Future<Item=Option<V>, Error=Error> + Send>>
            ("get")
    }

    pub fn get_blob(&self, ridp: &RID)
        -> impl Future<Item=Box<DivBuf>, Error=Error> + Send
    {
        self.e.was_called_returning::<*const RID,
            Box<Future<Item=Box<DivBuf>, Error=Error> + Send>>
            ("get_blob", ridp as *const RID)
    }

    pub fn expect_get_blob(&mut self) -> Method<*const RID,
        Box<Future<Item=Box<DivBuf>, Error=Error> + Send>>
    {
        self.e.expect::<*const RID,
            Box<Future<Item=Box<DivBuf>, Error=Error> + Send>>("get_blob")
    }

    pub fn insert(&self, k: K, v: V) -> impl Future<Item=Option<V>, Error=Error>
    {
        self.e.was_called_returning::<(K, V),
            Box<Future<Item=Option<V>, Error=Error> + Send>>("insert", (k, v))
    }

    pub fn expect_insert(&mut self) -> Method<(K, V),
        Box<Future<Item=Option<V>, Error=Error> + Send>>
    {
        self.e.expect::<(K, V), Box<Future<Item=Option<V>, Error=Error> + Send>>
            ("insert")
    }

    pub fn new() -> Self {
        Self {
            e: Expectations::new(),
            a: PhantomData,
            b: PhantomData,
        }
    }

    pub fn put_blob(&self, dbs: DivBufShared, compression: Compression)
        -> impl Future<Item=RID, Error=Error> + Send
    {
        self.e.was_called_returning::<(DivBufShared, Compression),
            Box<Future<Item=RID, Error=Error> + Send>>("put_blob",
            (dbs, compression))
    }

    pub fn expect_put_blob(&mut self) -> Method<(DivBufShared, Compression),
        Box<Future<Item=RID, Error=Error> + Send>>
    {
        self.e.expect::<(DivBufShared, Compression),
                        Box<Future<Item=RID, Error=Error> + Send>>
            ("put_blob")
    }

    pub fn range<R, T>(&self, range: R) -> impl Stream<Item=(K, V), Error=Error>
        where K: Borrow<T>,
              R: RangeBounds<T> + 'static,
              T: Ord + Clone + Send + 'static
    {
        self.e.was_called_returning::<R,
            Box<Stream<Item=(K, V), Error=Error> + Send>>
            ("range", range)
    }

    pub fn expect_range<R, T>(&mut self)
        -> Method<R, Box<Stream<Item=(K, V), Error=Error> + Send>>
        where K: Borrow<T>,
              R: RangeBounds<T> + 'static,
              T: Ord + Clone + Send + 'static
    {
        self.e.expect::<R, Box<Stream<Item=(K, V), Error=Error> + Send>>
            ("range")
    }

    pub fn range_delete<R, T>(&self, range: R)
        -> impl Future<Item=(), Error=Error>
        where K: Borrow<T>,
              R: RangeBounds<T> + 'static,
              T: Ord + Clone + Send + 'static
    {
        self.e.was_called_returning::<R,
            Box<Future<Item=(), Error=Error> + Send>>
            ("range_delete", range)
    }

    pub fn expect_range_delete<R, T>(&mut self)
        -> Method<R, Box<Future<Item=(), Error=Error> + Send>>
        where K: Borrow<T>,
              R: RangeBounds<T> + 'static,
              T: Ord + Clone + Send + 'static
    {
        self.e.expect::<R, Box<Future<Item=(), Error=Error> + Send>>
            ("range_delete")
    }

    pub fn remove(&self, k: K) -> impl Future<Item=Option<V>, Error=Error>
    {
        self.e.was_called_returning::<K,
            Box<Future<Item=Option<V>, Error=Error> + Send>>("remove", k)
    }

    pub fn expect_remove(&mut self) -> Method<K,
        Box<Future<Item=Option<V>, Error=Error> + Send>>
    {
        self.e.expect::<K, Box<Future<Item=Option<V>, Error=Error> + Send>>
            ("remove")
    }

    pub fn size(&self) -> LbaT {
        self.e.was_called_returning::<(), LbaT> ("size", ())
    }

    pub fn expect_size(&mut self) -> Method<(), LbaT> {
        self.e.expect::<(), LbaT>("size")
    }

    pub fn then(&mut self) -> &mut Self {
        self.e.then();
        self
    }
}

// XXX totally unsafe!  But Simulacrum doesn't support mocking Send traits.  So
// we have to cheat.  This works as long as the mocks are only used in
// single-threaded unit tests.
unsafe impl<K: Key, V: Value> Send for ReadOnlyDatasetMock<K, V> {}
unsafe impl<K: Key, V: Value> Sync for ReadOnlyDatasetMock<K, V> {}
unsafe impl<K: Key, V: Value> Send for ReadWriteDatasetMock<K, V> {}
unsafe impl<K: Key, V: Value> Sync for ReadWriteDatasetMock<K, V> {}
// LCOV_EXCL_STOP