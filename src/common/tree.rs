// vim: tw=80

//! COW B+-Trees, based on B-trees, Shadowing, and Clones[^CowBtrees]
//!
//! [^CowBtrees]: Rodeh, Ohad. "B-trees, shadowing, and clones." ACM Transactions on Storage (TOS) 3.4 (2008): 2.

use bincode;
use common::*;
use common::ddml::*;
use futures::future::{self, IntoFuture};
use futures::Future;
use futures_locks::*;
use nix::{Error, errno};
use serde::{Serialize, Serializer};
use serde::de::{self, Deserializer, DeserializeOwned, Visitor, MapAccess};
#[cfg(test)] use serde_yaml;
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::fmt::{self, Debug};
#[cfg(test)] use std::fmt::{Display, Formatter};
use std::marker::PhantomData;
use std::mem;
use std::rc::Rc;
use std::ops::{Deref, DerefMut};
use std::sync::atomic::{AtomicUsize, Ordering};
use tokio::executor::current_thread;

mod atomic_usize_serializer {
    use super::*;

    pub fn deserialize<'de, D>(d: D) -> Result<AtomicUsize, D::Error>
        where D: Deserializer<'de>
    {
        struct UsizeVisitor;

        impl<'de> Visitor<'de> for UsizeVisitor {
            type Value = usize;

            fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
                formatter.write_str("an integer between -2^31 and 2^31")
            }

            fn visit_u8<E>(self, value: u8) -> Result<usize, E>
                where E: de::Error
            {
                Ok(value as usize)
            }
            fn visit_u16<E>(self, value: u16) -> Result<usize, E>
                where E: de::Error
            {
                Ok(value as usize)
            }
            fn visit_u32<E>(self, value: u32) -> Result<usize, E>
                where E: de::Error
            {
                Ok(value as usize)
            }
            fn visit_u64<E>(self, value: u64) -> Result<usize, E>
                where E: de::Error
            {
                Ok(value as usize)
            }
        }
        d.deserialize_u64(UsizeVisitor).map(|x| AtomicUsize::new(x as usize))
    }

    pub fn serialize<S>(x: &AtomicUsize, s: S) -> Result<S::Ok, S::Error>
        where S: Serializer
    {
        s.serialize_u64(x.load(Ordering::Relaxed) as u64)
    }
}

#[cfg(test)]
/// Only exists so mockers can replace DDML
pub trait DDMLTrait {
    fn delete(&self, drp: &DRP);
    fn evict(&self, drp: &DRP);
    fn get(&self, drp: &DRP) -> Box<Future<Item=DivBuf, Error=Error>>;
    fn pop(&self, drp: &DRP) -> Box<Future<Item=DivBufShared, Error=Error>>;
    fn put(&self, uncompressed: DivBufShared, compression: Compression)
        -> (DRP, Box<Future<Item=(), Error=Error>>);
    fn sync_all(&self) -> Box<Future<Item=(), Error=Error>>;
}
#[cfg(test)]
pub type DDMLLike = Box<DDMLTrait>;
#[cfg(not(test))]
#[doc(hidden)]
pub type DDMLLike = DDML;

/// Anything that has a min_value method.  Too bad libstd doesn't define this.
pub trait MinValue {
    fn min_value() -> Self;
}

impl MinValue for u32 {
    fn min_value() -> Self {
        u32::min_value()
    }
}

pub trait Key: Copy + Debug + DeserializeOwned + Ord + MinValue + Serialize
    + 'static {}

impl<T> Key for T
where T: Copy + Debug + DeserializeOwned + Ord + MinValue + Serialize
    + 'static {}

pub trait Value: Copy + Debug + DeserializeOwned + Serialize + 'static {}

impl<T> Value for T
where T: Copy + Debug + DeserializeOwned + Serialize + 'static {}

#[derive(Debug, Deserialize, Serialize)]
#[serde(bound(deserialize = "K: DeserializeOwned, V: DeserializeOwned"))]
enum TreePtr<K: Key, V: Value> {
    /// Dirty btree nodes live only in RAM, not on disk or in cache.  Being
    /// RAM-resident, we don't need to store their checksums or lsizes.
    Mem(Box<Node<K, V>>),
    /// Direct Record Pointers point directly to a disk location
    DRP(DRP),
    /// Indirect Record Pointers point to the Record Indirection Table
    _IRP(u64)
}

impl<K: Key, V: Value> TreePtr<K, V> {
    fn as_drp(&self) -> Option<&DRP> {
        if let &TreePtr::DRP(ref drp) = self {
            Some(drp)
        } else {
            None
        }
    }

    fn as_mem(&self) -> Option<&Node<K, V>> {
        if let &TreePtr::Mem(ref mem) = self {
            Some(mem)
        } else {
            None
        }
    }

    fn as_mem_mut(&mut self) -> Option<&mut Node<K, V>> {
        if let &mut TreePtr::Mem(ref mut mem) = self {
            Some(mem)
        } else {
            None
        }
    }

    fn is_dirty(&self) -> bool {
        self.is_mem()
    }

    fn is_drp(&self) -> bool {
        if let &TreePtr::DRP(_) = self {
            true
        } else {
            false
        }
    }

    fn is_mem(&self) -> bool {
        if let &TreePtr::Mem(_) = self {
            true
        } else {
            false
        }
    }
}

mod treeptr_serializer {
    use super::*;

    pub(super) fn deserialize<'de, D, K, V>(deserializer: D)
        -> Result<RwLock<TreePtr<K, V>>, D::Error>
        where D: Deserializer<'de>, K: Key, V: Value
    {
        #[derive(Deserialize)]
        #[serde(field_identifier)]
        enum Field { Mem };

        struct TreePtrVisitor<K: Key, V: Value> {
            _k: PhantomData<K>,
            _v: PhantomData<V>
        }

        impl<'de, K: Key, V: Value> Visitor<'de> for TreePtrVisitor<K, V> {
            type Value = RwLock<TreePtr<K, V>>;

            fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
                formatter.write_str("enum TreePtr")
            }

            fn visit_map<Q>(self, mut map: Q)
                -> Result<RwLock<TreePtr<K, V>>, Q::Error>
                where Q: MapAccess<'de>
            {
                let mut ptr = None;
                while let Some(key) = map.next_key()? {
                    match key {
                        Field::Mem => {
                            if ptr.is_some() {
                                return Err(de::Error::duplicate_field("mem"));
                            }
                            ptr = Some(
                                RwLock::new(
                                    TreePtr::Mem(
                                        Box::new(map.next_value()?
                                        )
                                    )
                                )
                            );
                        }
                    }
                }
                ptr.ok_or(de::Error::missing_field("mem"))
            }
        }

        const FIELDS: &'static [&'static str] = &["Mem"];
        let visitor = TreePtrVisitor{_k: PhantomData, _v: PhantomData};
        deserializer.deserialize_struct("Mem", FIELDS, visitor)
    }

    pub(super) fn serialize<S, K, V>(lock: &RwLock<TreePtr<K, V>>,
                                     serializer: S) -> Result<S::Ok, S::Error>
        where S: Serializer, K: Key, V: Value {

        let guard = current_thread::block_on_all(lock.read()).unwrap();
        (*guard).serialize(serializer)
    }
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(bound(deserialize = "K: DeserializeOwned, V: DeserializeOwned"))]
struct LeafNode<K: Key, V> {
    items: BTreeMap<K, V>
}

impl<K: Key, V: Value> LeafNode<K, V> {
    fn split(&mut self) -> (K, LeafNode<K, V>) {
        // Split the node in two.  Make the left node larger, on the assumption
        // that we're more likely to insert into the right node than the left
        // one.
        let half = div_roundup(self.items.len(), 2);
        let cutoff = *self.items.keys().nth(half).unwrap();
        let new_items = self.items.split_off(&cutoff);
        (cutoff, LeafNode{items: new_items})
    }
}

impl<K: Key, V: Value> LeafNode<K, V> {
    fn insert(&mut self, k: K, v: V) -> Option<V> {
        self.items.insert(k, v)
    }

    fn lookup(&self, k: K) -> Result<V, Error> {
        self.items.get(&k)
            .cloned()
            .ok_or(Error::Sys(errno::Errno::ENOENT))
    }

    fn remove(&mut self, k: K) -> Option<V> {
        self.items.remove(&k)
    }
}

/// Guard that holds the Node lock object for reading
enum TreeReadGuard<K: Key, V: Value> {
    Mem(RwLockReadGuard<TreePtr<K, V>>),
    _DRP(RwLockReadGuard<TreePtr<K, V>>, Node<K, V>)
}

impl<K: Key, V: Value> Deref for TreeReadGuard<K, V> {
    type Target = Node<K, V>;

    fn deref(&self) -> &Self::Target {
        match self {
            &TreeReadGuard::Mem(ref guard) => guard.as_mem().unwrap(),
            _ => unimplemented!()
        }
    }
}

/// Guard that holds the Node lock object for writing
enum TreeWriteGuard<K: Key, V: Value> {
    Mem(RwLockWriteGuard<TreePtr<K, V>>),
    _DRP(RwLockReadGuard<TreePtr<K, V>>, Node<K, V>)
}

impl<K: Key, V: Value> Deref for TreeWriteGuard<K, V> {
    type Target = Node<K, V>;

    fn deref(&self) -> &Self::Target {
        match self {
            &TreeWriteGuard::Mem(ref guard) => guard.as_mem().unwrap(),
            _ => unimplemented!()
        }
    }
}

impl<K: Key, V: Value> DerefMut for TreeWriteGuard<K, V> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        match self {
            &mut TreeWriteGuard::Mem(ref mut guard) =>
                guard.as_mem_mut().unwrap(),
            _ => unimplemented!()
        }
    }
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(bound(deserialize = "K: DeserializeOwned"))]
struct IntElem<K: Key + DeserializeOwned, V: Value> {
    key: K,
    #[serde(with = "treeptr_serializer")]
    ptr: RwLock<TreePtr<K, V>>
}

impl<'a, K: Key, V: Value> IntElem<K, V> {
    /// Is the child node dirty?  That is, does it differ from the on-disk
    /// version?
    fn is_dirty(&mut self) -> bool {
        self.ptr.get_mut().unwrap().is_dirty()
    }

    fn rlock(&self, ddml: &'a DDMLLike)
        -> Box<Future<Item=TreeReadGuard<K, V>, Error=Error> + 'a>
    {
        let gfut = self.ptr.read()
        .map_err(|_| Error::Sys(errno::Errno::EPIPE))
        .and_then(move |g| {
            if g.is_mem() {
                let fut: Box<Future<Item=TreeReadGuard<K, V>, Error=Error>> =
                    Box::new(future::ok(TreeReadGuard::Mem(g)));
                fut
            } else if g.is_drp() {
                let fut: Box<Future<Item=TreeReadGuard<K, V>, Error=Error>> =
                Box::new(
                ddml.get(&g.as_drp().unwrap())
                    .map(|_db| {
                        // TODO: deserialize the Node
                        unimplemented!()
                    }));
                fut
            } else {
                unimplemented!()
            }
        });
        Box::new(gfut)
    }

    fn xlock(&self, ddml: &'a DDMLLike)
        -> Box<Future<Item=TreeWriteGuard<K, V>, Error=Error> + 'a>
    {
        let gfut = self.ptr.write()
        .map_err(|_| Error::Sys(errno::Errno::EPIPE))
        .and_then(move |g| {
            if g.is_mem() {
                let fut: Box<Future<Item=TreeWriteGuard<K, V>, Error=Error>> =
                    Box::new(future::ok(TreeWriteGuard::Mem(g)));
                fut
            } else if g.is_drp() {
                let fut: Box<Future<Item=TreeWriteGuard<K, V>, Error=Error>> =
                Box::new(
                ddml.get(&g.as_drp().unwrap())
                    .map(|_db| {
                        // TODO: deserialize the Node
                        unimplemented!()
                    }));
                fut
            } else {
                unimplemented!()
            }
        });
        Box::new(gfut)
     }
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(bound(deserialize = "K: DeserializeOwned"))]
struct IntNode<K: Key, V: Value> {
    children: Vec<IntElem<K, V>>
}

impl<K: Key, V: Value> IntNode<K, V> {
    fn position(&self, k: &K) -> usize {
        // Find rightmost child whose key is less than or equal to k
        self.children
            .binary_search_by_key(k, |ref child| child.key)
            .unwrap_or_else(|k| k - 1)
    }

    fn split(&mut self) -> (K, IntNode<K, V>) {
        // Split the node in two.  Make the left node larger, on the assumption
        // that we're more likely to insert into the right node than the left
        // one.
        let cutoff = div_roundup(self.children.len(), 2);
        let new_children = self.children.split_off(cutoff);
        (new_children[0].key, IntNode{children: new_children})
    }
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(bound(deserialize = "K: DeserializeOwned"))]
enum Node<K: Key, V: Value> {
    Leaf(LeafNode<K, V>),
    Int(IntNode<K, V>)
}

impl<K: Key, V: Value> Node<K, V> {
    fn as_int(&self) -> Option<&IntNode<K, V>> {
        if let &Node::Int(ref int) = self {
            Some(int)
        } else {
            None
        }
    }

    fn as_int_mut(&mut self) -> Option<&mut IntNode<K, V>> {
        if let &mut Node::Int(ref mut int) = self {
            Some(int)
        } else {
            None
        }
    }

    fn as_leaf_mut(&mut self) -> Option<&mut LeafNode<K, V>> {
        if let &mut Node::Leaf(ref mut leaf) = self {
            Some(leaf)
        } else {
            None
        }
    }

    /// Can this child be merged with `other` without violating constraints?
    fn can_merge(&self, other: &Node<K, V>, max_fanout: usize) -> bool {
        self.len() + other.len() <= max_fanout
    }

    /// Return this `Node`s lower bound key, suitable for use in its parent's
    /// `children` array.
    fn key(&self) -> K {
        match self {
            &Node::Leaf(ref leaf) => *leaf.items.keys().nth(0).unwrap(),
            &Node::Int(ref int) => int.children[0].key,
        }
    }

    /// Number of children or items in this `Node`
    fn len(&self) -> usize {
        match self {
            &Node::Leaf(ref leaf) => leaf.items.len(),
            &Node::Int(ref int) => int.children.len()
        }
    }

    /// Should this node be fixed because it's too small?
    fn should_fix(&self, min_fanout: usize) -> bool {
        let len = self.len();
        debug_assert!(len >= min_fanout,
                      "Underfull nodes shouldn't be possible");
        len <= min_fanout
    }

    /// Should this node be split because it's too big?
    fn should_split(&self, max_fanout: usize) -> bool {
        let len = self.len();
        debug_assert!(len <= max_fanout,
                      "Overfull nodes shouldn't be possible");
        len >= max_fanout
    }

    fn split(&mut self) -> (K, Node<K, V>) {
        match *self {
            Node::Leaf(ref mut leaf) => {
                let (k, new_leaf) = leaf.split();
                (k, Node::Leaf(new_leaf))
            },
            Node::Int(ref mut int) => {
                let (k, new_int) = int.split();
                (k, Node::Int(new_int))
            },

        }
    }

    /// Merge all of `other`'s data into `self`.  Afterwards, `other` may be
    /// deleted.
    fn merge(&mut self, other: &mut Node<K, V>) {
        match *self {
            Node::Int(ref mut int) =>
                int.children.append(&mut other.as_int_mut().unwrap().children),
            Node::Leaf(ref mut leaf) =>
                leaf.items.append(&mut other.as_leaf_mut().unwrap().items),
        }
    }

    /// Take `other`'s highest keys and merge them into ourself
    fn take_high_keys(&mut self, other: &mut Node<K, V>) {
        let keys_to_share = (other.len() - self.len()) / 2;
        match *self {
            Node::Int(ref mut int) => {
                let other_children = &mut other.as_int_mut().unwrap().children;
                let cutoff_idx = other_children.len() - keys_to_share;
                let mut other_right_half =
                    other_children.split_off(cutoff_idx);
                int.children.splice(0..0, other_right_half.into_iter());
            },
            Node::Leaf(ref mut leaf) => {
                let other_items = &mut other.as_leaf_mut().unwrap().items;
                let cutoff_idx = other_items.len() - keys_to_share;
                let cutoff = *other_items.keys().nth(cutoff_idx).unwrap();
                let mut other_right_half = other_items.split_off(&cutoff);
                leaf.items.append(&mut other_right_half);
            }
        }
    }

    /// Take `other`'s lowest keys and merge them into ourself
    fn take_low_keys(&mut self, other: &mut Node<K, V>) {
        let keys_to_share = (other.len() - self.len()) / 2;
        match *self {
            Node::Int(ref mut int) => {
                let other_children = &mut other.as_int_mut().unwrap().children;
                let other_left_half = other_children.drain(0..keys_to_share);
                let nchildren = int.children.len();
                int.children.splice(nchildren.., other_left_half);
            },
            Node::Leaf(ref mut leaf) => {
                let other_items = &mut other.as_leaf_mut().unwrap().items;
                let cutoff = *other_items.keys().nth(keys_to_share).unwrap();
                let other_right_half = other_items.split_off(&cutoff);
                let mut other_left_half =
                    mem::replace(other_items, other_right_half);
                leaf.items.append(&mut other_left_half);
            }
        }
    }
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(bound(deserialize = "K: DeserializeOwned"))]
struct Inner<K: Key, V: Value> {
    /// Tree height.  1 if the Tree consists of a single Leaf node.
    // Use atomics so it can be modified from an immutable reference.  Accesses
    // should be very rare, so performance is not a concern.
    #[serde(with = "atomic_usize_serializer")]
    height: AtomicUsize,
    /// Minimum node fanout.  Smaller nodes will be merged, or will steal
    /// children from their neighbors.
    min_fanout: usize,
    /// Maximum node fanout.  Larger nodes will be split.
    max_fanout: usize,
    /// Maximum node size in bytes.  Larger nodes will be split or their message
    /// buffers flushed
    _max_size: usize,
    /// Root node
    root: IntElem<K, V>
}

/// In-memory representation of a COW B+-Tree
///
/// # Generic Parameters
///
/// *`K`:   Key type.  Must be ordered and copyable; should be compact
/// *`V`:   Value type in the leaves.
pub struct Tree<K: Key, V: Value> {
    ddml: DDMLLike,
    i: Inner<K, V>
}

impl<'a, K: Key, V: Value> Tree<K, V> {
    #[cfg(not(test))]
    pub fn create(ddml: DDML) -> Self {
        Tree::new(ddml,
                  4,        // BetrFS's min fanout
                  16,       // BetrFS's max fanout
                  1<<22,    // BetrFS's max size
        )
    }

    #[cfg(test)]
    pub fn from_str(ddml: DDMLLike, s: &str) -> Self {
        let i: Inner<K, V> = serde_yaml::from_str(s).unwrap();
        Tree{ddml, i}
    }

    /// Insert value `v` into the tree at key `k`, returning the previous value
    /// for that key, if any.
    pub fn insert(&'a self, k: K, v: V)
        -> Box<Future<Item=Option<V>, Error=Error> + 'a> {

        Box::new(
            self.i.root.xlock(&self.ddml)
                .map_err(|_| Error::Sys(errno::Errno::EPIPE))
                .and_then(move |root| {
                    self.insert_locked(root, k, v)
            })
        )
    }

    /// Insert value `v` into an internal node.  The internal node and its
    /// relevant child must both be already locked.
    fn insert_int(&'a self, mut parent: TreeWriteGuard<K, V>,
                  child_idx: usize,
                  mut child: TreeWriteGuard<K, V>, k: K, v: V)
        -> Box<Future<Item=Option<V>, Error=Error> + 'a> {

        // First, split the node, if necessary
        if (*child).should_split(self.i.max_fanout) {
            let (new_key, new_node) = child.split();
            let new_ptr = RwLock::new(TreePtr::Mem(Box::new(new_node)));
            let new_elem = IntElem{key: new_key, ptr: new_ptr};
            parent.as_int_mut().unwrap()
                .children.insert(child_idx + 1, new_elem);
            // Reinsert into the parent, which will choose the correct child
            self.insert_no_split(parent, k, v)
        } else {
            drop(parent);
            self.insert_no_split(child, k, v)
        }
    }

    /// Helper for `insert`.  Handles insertion once the tree is locked
    fn insert_locked(&'a self, mut root: TreeWriteGuard<K, V>, k: K, v: V)
        -> Box<Future<Item=Option<V>, Error=Error> + 'a> {

        // First, split the root node, if necessary
        if root.should_split(self.i.max_fanout) {
            let (new_key, new_node) = root.split();
            let new_ptr = RwLock::new(TreePtr::Mem(Box::new(new_node)));
            let new_elem = IntElem{key: new_key, ptr: new_ptr};
            let new_root = Node::Int(
                IntNode {
                    children: vec![new_elem]
                }
            );
            let old_root = mem::replace(root.deref_mut(), new_root);
            let old_ptr = RwLock::new(TreePtr::Mem(Box::new(old_root)));
            let old_elem = IntElem{ key: K::min_value(), ptr: old_ptr };
            root.as_int_mut().unwrap().children.insert(0, old_elem);
            self.i.height.fetch_add(1, Ordering::Relaxed);
        }

        self.insert_no_split(root, k, v)
    }

    fn insert_no_split(&'a self, mut node: TreeWriteGuard<K, V>, k: K, v: V)
        -> Box<Future<Item=Option<V>, Error=Error> + 'a> {

        let (child_idx, child_fut) = match *node {
            Node::Leaf(ref mut leaf) => {
                return Box::new(Ok(leaf.insert(k, v)).into_future())
            },
            Node::Int(ref int) => {
                let child_idx = int.position(&k);
                let fut = int.children[child_idx].xlock(&self.ddml);
                (child_idx, fut)
            }
        };
        Box::new(child_fut.and_then(move |child| {
                self.insert_int(node, child_idx, child, k, v)
            })
        )
    }

    /// Lookup the value of key `k`.  Return an error if no value is present.
    pub fn lookup(&'a self, k: K) -> Box<Future<Item=V, Error=Error> + 'a> {
        Box::new(
            self.i.root.rlock(&self.ddml)
                .map_err(|_| Error::Sys(errno::Errno::EPIPE))
                .and_then(move |root| self.lookup_node(root, k))
        )
    }

    /// Lookup the value of key `k` in a node, which must already be locked.
    fn lookup_node(&'a self, node: TreeReadGuard<K, V>, k: K)
        -> Box<Future<Item=V, Error=Error> + 'a> {

        let next_node_fut = match *node {
            Node::Leaf(ref leaf) => {
                return Box::new(leaf.lookup(k).into_future())
            },
            Node::Int(ref int) => {
                let child_elem = &int.children[int.position(&k)];
                child_elem.rlock(&self.ddml)
            }
        };
        drop(node);
        Box::new(
            next_node_fut
            .and_then(move |next_node| self.lookup_node(next_node, k))
        )
    }

    #[cfg(any(not(test), feature = "mocks"))]
    fn new(ddml: DDMLLike, min_fanout: usize, max_fanout: usize,
           max_size: usize) -> Self
    {
        let i: Inner<K, V> = Inner {
            height: AtomicUsize::new(1),
            min_fanout, max_fanout,
            _max_size: max_size,
            root: IntElem{
                key: K::min_value(),
                ptr: RwLock::new(
                    TreePtr::Mem(
                        Box::new(
                            Node::Leaf(
                                LeafNode{
                                    items: BTreeMap::new()
                                }
                            )
                        )
                    )
                )
            }
        };
        Tree{ ddml, i }
    }

    /// Remove and return the value at key `k`, if any.
    pub fn remove(&'a self, k: K)
        -> Box<Future<Item=Option<V>, Error=Error> + 'a> {

        Box::new(
            self.i.root.xlock(&self.ddml)
                .map_err(|_| Error::Sys(errno::Errno::EPIPE))
                .and_then(move |root| {
                    self.remove_locked(root, k)
            })
        )
    }

    /// Remove key `k` from an internal node.  The internal node and its
    /// relevant child must both be already locked.
    fn remove_int(&'a self, mut parent: TreeWriteGuard<K, V>,
                  child_idx: usize, mut child: TreeWriteGuard<K, V>, k: K)
        -> Box<Future<Item=Option<V>, Error=Error> + 'a> {

        // First, fix the node, if necessary
        if child.should_fix(self.i.min_fanout) {
            // Outline:
            // First, try to merge with the right sibling
            // Then, try to steal keys from the right sibling
            // Then, try to merge with the left sibling
            // Then, try to steal keys from the left sibling
            let nchildren = parent.as_int().unwrap().children.len();
            let (fut, right) = if child_idx < nchildren - 1 {
                (parent.as_int_mut().unwrap().children[child_idx + 1]
                 .xlock(&self.ddml),
                 true)
            } else {
                (parent.as_int_mut().unwrap().children[child_idx - 1]
                 .xlock(&self.ddml),
                 false)
            };
            Box::new(
                fut.map(move |mut sibling| {
                    if right {
                        if child.can_merge(&sibling, self.i.max_fanout) {
                            child.merge(&mut sibling);
                            parent.as_int_mut().unwrap()
                                .children.remove(child_idx + 1);
                        } else {
                            child.take_low_keys(&mut sibling);
                            parent.as_int_mut().unwrap().children[child_idx+1]
                                .key = sibling.key();
                        }
                    } else {
                        if sibling.can_merge(&child, self.i.max_fanout) {
                            sibling.merge(&mut child);
                            parent.as_int_mut().unwrap()
                                .children.remove(child_idx);
                        } else {
                            child.take_high_keys(&mut sibling);
                            parent.as_int_mut().unwrap().children[child_idx]
                                .key = child.key();
                        }
                    };
                    parent
                }).and_then(move |parent| self.remove_no_fix(parent, k))
            )
        } else {
            drop(parent);
            self.remove_no_fix(child, k)
        }
    }

    /// Helper for `remove`.  Handles removal once the tree is locked
    fn remove_locked(&'a self, mut root: TreeWriteGuard<K, V>, k: K)
        -> Box<Future<Item=Option<V>, Error=Error> + 'a> {

        // First, fix the root node, if necessary
        let new_root = if let Node::Int(ref mut int) = *root {
            if int.children.len() == 1 {
                // Merge root node with its child
                let child = int.children.pop().unwrap();
                Some(match child.ptr.try_unwrap().unwrap() {
                    TreePtr::Mem(node) => node,
                    _ => unimplemented!()
                })
            } else {
                None
            }
        } else {
            None
        };
        if new_root.is_some() {
            mem::replace(root.deref_mut(), *new_root.unwrap());
            self.i.height.fetch_sub(1, Ordering::Relaxed);
        }

        self.remove_no_fix(root, k)
    }

    /// Remove key `k` from a node, but don't try to fixup the node.
    fn remove_no_fix(&'a self, mut node: TreeWriteGuard<K, V>, k: K)
        -> Box<Future<Item=Option<V>, Error=Error> + 'a> {

        let (child_idx, child_fut) = match *node {
            Node::Leaf(ref mut leaf) => {
                return Box::new(Ok(leaf.remove(k)).into_future())
            },
            Node::Int(ref int) => {
                let child_idx = int.position(&k);
                let fut = int.children[child_idx].xlock(&self.ddml);
                (child_idx, fut)
            }
        };
        Box::new(child_fut.and_then(move |child| {
                self.remove_int(node, child_idx, child, k)
            })
        )
    }

    /// Sync all records written so far to stable storage.
    pub fn sync_all(&'a self) -> Box<Future<Item=(), Error=Error> + 'a> {
        Box::new(
            self.i.root.xlock(&self.ddml).and_then(move |root| {
                // TODO: figure out what to do with the DRP
                self.write_node(root)
            }).and_then(move |_drp| {
                self.ddml.sync_all()
            })
        )
    }

    fn write_leaf(&'a self, node: &LeafNode<K, V>)
        -> Box<Future<Item=DRP, Error=Error> + 'a>
    {
        let buf = DivBufShared::from(bincode::serialize(&node.items).unwrap());
        let (drp, fut) = self.ddml.put(buf, Compression::None);
        Box::new(fut.map(move |_| drp))
    }

    fn write_node(&'a self, node: TreeWriteGuard<K, V>)
        -> Box<Future<Item=DRP, Error=Error> + 'a>
    {
        if let Node::Leaf(ref leaf) = *node {
            return self.write_leaf(leaf);
        }
        // Rust's borrow checker doesn't understand that children_fut will
        // complete before its continuation will run, so it won't let node be
        // borrowed in both places.  So we'll have to use RefCell to allow
        // dynamic borrowing and Rc to allow moving into both closures.
        let rnode = Rc::new(RefCell::new(node));
        let rnode2 = rnode.clone();
        let nchildren = rnode.borrow().as_int().unwrap().children.len();
        let children_fut = (0..nchildren)
        .filter_map(move |idx| {
            if rnode.borrow_mut().as_int_mut().unwrap()
                .children[idx].is_dirty()
            {
                let rnode3 = rnode.clone();
                Some(
                    rnode.borrow_mut().as_int_mut().unwrap().children[idx]
                    .xlock(&self.ddml)
                    .and_then(move |guard| self.write_node(guard))
                    .map(move |drp| {
                        rnode3.borrow_mut().as_int_mut().unwrap().children[idx]
                            .ptr = RwLock::new(TreePtr::DRP(drp));
                    })
                )
            } else {
                None
            }
        })
        .collect::<Vec<_>>();
        Box::new(
            future::join_all(children_fut)
            .and_then(move |_| {
                let buf = DivBufShared::from(bincode::serialize(
                        &rnode2.borrow().as_int().unwrap().children).unwrap());
                let (drp, fut) = self.ddml.put(buf, Compression::None);
                fut.map(move |_| drp)
            })
        )
    }
}

#[cfg(test)]
impl<K: Key, V: Value> Display for Tree<K, V> {
    fn fmt(&self, f: &mut Formatter) -> fmt::Result {
        f.write_str(&serde_yaml::to_string(&self.i).unwrap())
    }
}



#[cfg(test)]
#[cfg(feature = "mocks")]
mod t {

use super::*;
use futures::future;
use mockers::Scenario;

mock!{
    MockDDML,
    self,
    trait DDMLTrait {
        fn delete(&self, drp: &DRP);
        fn evict(&self, drp: &DRP);
        fn get(&self, drp: &DRP) -> Box<Future<Item=DivBuf, Error=Error>>;
        fn pop(&self, drp: &DRP) -> Box<Future<Item=DivBufShared, Error=Error>>;
        fn put(&self, uncompressed: DivBufShared, compression: Compression)
            -> (DRP, Box<Future<Item=(), Error=Error>>);
        fn sync_all(&self) -> Box<Future<Item=(), Error=Error>>;
    }
}

#[test]
fn insert() {
    let ddml = Box::new(Scenario::new().create_mock::<MockDDML>());
    let tree: Tree<u32, f32> = Tree::new(ddml, 2, 5, 1<<22);
    let r = current_thread::block_on_all(tree.insert(0, 0.0));
    assert_eq!(r, Ok(None));
    assert_eq!(format!("{}", tree),
r#"---
height: 1
min_fanout: 2
max_fanout: 5
_max_size: 4194304
root:
  key: 0
  ptr:
    Mem:
      Leaf:
        items:
          0: 0.0"#);
}

#[test]
fn insert_dup() {
    let ddml = Box::new(Scenario::new().create_mock::<MockDDML>());
    let tree = Tree::from_str(ddml, r#"
---
height: 1
min_fanout: 2
max_fanout: 5
_max_size: 4194304
root:
  key: 0
  ptr:
    Mem:
      Leaf:
        items:
          0: 0.0
"#);
    let r = current_thread::block_on_all(tree.insert(0, 100.0));
    assert_eq!(r, Ok(Some(0.0)));
    assert_eq!(format!("{}", tree),
r#"---
height: 1
min_fanout: 2
max_fanout: 5
_max_size: 4194304
root:
  key: 0
  ptr:
    Mem:
      Leaf:
        items:
          0: 100.0"#);
}

/// Insert a key that splits a non-root interior node
#[test]
fn insert_split_int() {
    let ddml = Box::new(Scenario::new().create_mock::<MockDDML>());
    let tree = Tree::from_str(ddml, r#"
---
height: 3
min_fanout: 2
max_fanout: 5
_max_size: 4194304
root:
  key: 0
  ptr:
    Mem:
      Int:
        children:
          - key: 0
            ptr:
              Mem:
                Int:
                  children:
                    - key: 0
                      ptr:
                        Mem:
                          Leaf:
                            items:
                              0: 0.0
                              1: 1.0
                              2: 2.0
                    - key: 3
                      ptr:
                        Mem:
                          Leaf:
                            items:
                              3: 3.0
                              4: 4.0
                              5: 5.0
                    - key: 6
                      ptr:
                        Mem:
                          Leaf:
                            items:
                              6: 6.0
                              7: 7.0
                              8: 8.0
          - key: 9
            ptr:
              Mem:
                Int:
                  children:
                    - key: 9
                      ptr:
                        Mem:
                          Leaf:
                            items:
                              9: 9.0
                              10: 10.0
                              11: 11.0
                    - key: 12
                      ptr:
                        Mem:
                          Leaf:
                            items:
                              12: 12.0
                              13: 13.0
                              14: 14.0
                    - key: 15
                      ptr:
                        Mem:
                          Leaf:
                            items:
                              15: 15.0
                              16: 16.0
                              17: 17.0
                    - key: 18
                      ptr:
                        Mem:
                          Leaf:
                            items:
                              18: 18.0
                              19: 19.0
                              20: 20.0
                    - key: 21
                      ptr:
                        Mem:
                          Leaf:
                            items:
                              21: 21.0
                              22: 22.0
                              23: 23.0"#);
    let r2 = current_thread::block_on_all(tree.insert(24, 24.0));
    assert!(r2.is_ok());
    assert_eq!(format!("{}", &tree),
r#"---
height: 3
min_fanout: 2
max_fanout: 5
_max_size: 4194304
root:
  key: 0
  ptr:
    Mem:
      Int:
        children:
          - key: 0
            ptr:
              Mem:
                Int:
                  children:
                    - key: 0
                      ptr:
                        Mem:
                          Leaf:
                            items:
                              0: 0.0
                              1: 1.0
                              2: 2.0
                    - key: 3
                      ptr:
                        Mem:
                          Leaf:
                            items:
                              3: 3.0
                              4: 4.0
                              5: 5.0
                    - key: 6
                      ptr:
                        Mem:
                          Leaf:
                            items:
                              6: 6.0
                              7: 7.0
                              8: 8.0
          - key: 9
            ptr:
              Mem:
                Int:
                  children:
                    - key: 9
                      ptr:
                        Mem:
                          Leaf:
                            items:
                              9: 9.0
                              10: 10.0
                              11: 11.0
                    - key: 12
                      ptr:
                        Mem:
                          Leaf:
                            items:
                              12: 12.0
                              13: 13.0
                              14: 14.0
                    - key: 15
                      ptr:
                        Mem:
                          Leaf:
                            items:
                              15: 15.0
                              16: 16.0
                              17: 17.0
          - key: 18
            ptr:
              Mem:
                Int:
                  children:
                    - key: 18
                      ptr:
                        Mem:
                          Leaf:
                            items:
                              18: 18.0
                              19: 19.0
                              20: 20.0
                    - key: 21
                      ptr:
                        Mem:
                          Leaf:
                            items:
                              21: 21.0
                              22: 22.0
                              23: 23.0
                              24: 24.0"#);
}

/// Insert a key that splits a non-root leaf node
#[test]
fn insert_split_leaf() {
    let ddml = Box::new(Scenario::new().create_mock::<MockDDML>());
    let tree = Tree::from_str(ddml, r#"
---
height: 2
min_fanout: 2
max_fanout: 5
_max_size: 4194304
root:
  key: 0
  ptr:
    Mem:
      Int:
        children:
          - key: 0
            ptr:
              Mem:
                Leaf:
                  items:
                    0: 0.0
                    1: 1.0
                    2: 2.0
          - key: 3
            ptr:
              Mem:
                Leaf:
                  items:
                    3: 3.0
                    4: 4.0
                    5: 5.0
                    6: 6.0
                    7: 7.0
"#);
    let r2 = current_thread::block_on_all(tree.insert(8, 8.0));
    assert!(r2.is_ok());
    assert_eq!(format!("{}", tree),
r#"---
height: 2
min_fanout: 2
max_fanout: 5
_max_size: 4194304
root:
  key: 0
  ptr:
    Mem:
      Int:
        children:
          - key: 0
            ptr:
              Mem:
                Leaf:
                  items:
                    0: 0.0
                    1: 1.0
                    2: 2.0
          - key: 3
            ptr:
              Mem:
                Leaf:
                  items:
                    3: 3.0
                    4: 4.0
                    5: 5.0
          - key: 6
            ptr:
              Mem:
                Leaf:
                  items:
                    6: 6.0
                    7: 7.0
                    8: 8.0"#);
}

/// Insert a key that splits the root IntNode
#[test]
fn insert_split_root_int() {
    let ddml = Box::new(Scenario::new().create_mock::<MockDDML>());
    let tree = Tree::from_str(ddml, r#"
---
height: 2
min_fanout: 2
max_fanout: 5
_max_size: 4194304
root:
  key: 0
  ptr:
    Mem:
      Int:
        children:
          - key: 0
            ptr:
              Mem:
                Leaf:
                  items:
                    0: 0.0
                    1: 1.0
                    2: 2.0
          - key: 3
            ptr:
              Mem:
                Leaf:
                  items:
                    3: 3.0
                    4: 4.0
                    5: 5.0
          - key: 6
            ptr:
              Mem:
                Leaf:
                  items:
                    6: 6.0
                    7: 7.0
                    8: 8.0
          - key: 9
            ptr:
              Mem:
                Leaf:
                  items:
                    9: 9.0
                    10: 10.0
                    11: 11.0
          - key: 12
            ptr:
              Mem:
                Leaf:
                  items:
                    12: 12.0
                    13: 13.0
                    14: 14.0
"#);
    let r2 = current_thread::block_on_all(tree.insert(15, 15.0));
    assert!(r2.is_ok());
    assert_eq!(format!("{}", &tree),
r#"---
height: 3
min_fanout: 2
max_fanout: 5
_max_size: 4194304
root:
  key: 0
  ptr:
    Mem:
      Int:
        children:
          - key: 0
            ptr:
              Mem:
                Int:
                  children:
                    - key: 0
                      ptr:
                        Mem:
                          Leaf:
                            items:
                              0: 0.0
                              1: 1.0
                              2: 2.0
                    - key: 3
                      ptr:
                        Mem:
                          Leaf:
                            items:
                              3: 3.0
                              4: 4.0
                              5: 5.0
                    - key: 6
                      ptr:
                        Mem:
                          Leaf:
                            items:
                              6: 6.0
                              7: 7.0
                              8: 8.0
          - key: 9
            ptr:
              Mem:
                Int:
                  children:
                    - key: 9
                      ptr:
                        Mem:
                          Leaf:
                            items:
                              9: 9.0
                              10: 10.0
                              11: 11.0
                    - key: 12
                      ptr:
                        Mem:
                          Leaf:
                            items:
                              12: 12.0
                              13: 13.0
                              14: 14.0
                              15: 15.0"#);
}

/// Insert a key that splits the root leaf node
#[test]
fn insert_split_root_leaf() {
    let ddml = Box::new(Scenario::new().create_mock::<MockDDML>());
    let tree = Tree::from_str(ddml, r#"
---
height: 1
min_fanout: 2
max_fanout: 5
_max_size: 4194304
root:
  key: 0
  ptr:
    Mem:
      Leaf:
        items:
          0: 0.0
          1: 1.0
          2: 2.0
          3: 3.0
          4: 4.0
"#);
    let r2 = current_thread::block_on_all(tree.insert(5, 5.0));
    assert!(r2.is_ok());
    assert_eq!(format!("{}", &tree),
r#"---
height: 2
min_fanout: 2
max_fanout: 5
_max_size: 4194304
root:
  key: 0
  ptr:
    Mem:
      Int:
        children:
          - key: 0
            ptr:
              Mem:
                Leaf:
                  items:
                    0: 0.0
                    1: 1.0
                    2: 2.0
          - key: 3
            ptr:
              Mem:
                Leaf:
                  items:
                    3: 3.0
                    4: 4.0
                    5: 5.0"#);
}

#[test]
fn lookup() {
    let ddml = Box::new(Scenario::new().create_mock::<MockDDML>());
    let tree: Tree<u32, f32> = Tree::new(ddml, 2, 5, 1<<22);
    let r = current_thread::block_on_all(future::lazy(|| {
        tree.insert(0, 0.0)
            .and_then(|_| tree.lookup(0))
    }));
    assert_eq!(r, Ok(0.0));
}

#[test]
fn lookup_nonexistent() {
    let ddml = Box::new(Scenario::new().create_mock::<MockDDML>());
    let tree: Tree<u32, f32> = Tree::new(ddml, 2, 5, 1<<22);
    let r = current_thread::block_on_all(tree.lookup(0));
    assert_eq!(r, Err(Error::Sys(errno::Errno::ENOENT)))
}

#[test]
fn remove_last_key() {
    let ddml = Box::new(Scenario::new().create_mock::<MockDDML>());
    let tree = Tree::from_str(ddml, r#"
---
height: 1
min_fanout: 2
max_fanout: 5
_max_size: 4194304
root:
  key: 0
  ptr:
    Mem:
      Leaf:
        items:
          0: 0.0
"#);
    let r = current_thread::block_on_all(tree.remove(0));
    assert_eq!(r, Ok(Some(0.0)));
    assert_eq!(format!("{}", tree),
r#"---
height: 1
min_fanout: 2
max_fanout: 5
_max_size: 4194304
root:
  key: 0
  ptr:
    Mem:
      Leaf:
        items: {}"#);
}

#[test]
fn remove_from_leaf() {
    let ddml = Box::new(Scenario::new().create_mock::<MockDDML>());
    let tree = Tree::from_str(ddml, r#"
---
height: 1
min_fanout: 2
max_fanout: 5
_max_size: 4194304
root:
  key: 0
  ptr:
    Mem:
      Leaf:
        items:
          0: 0.0
          1: 1.0
          2: 2.0
"#);
    let r = current_thread::block_on_all(tree.remove(1));
    assert_eq!(r, Ok(Some(1.0)));
    assert_eq!(format!("{}", tree),
r#"---
height: 1
min_fanout: 2
max_fanout: 5
_max_size: 4194304
root:
  key: 0
  ptr:
    Mem:
      Leaf:
        items:
          0: 0.0
          2: 2.0"#);
}

#[test]
fn remove_and_merge_int_left() {
    let ddml = Box::new(Scenario::new().create_mock::<MockDDML>());
    let tree: Tree<u32, f32> = Tree::from_str(ddml, r#"
---
height: 3
min_fanout: 2
max_fanout: 5
_max_size: 4194304
root:
  key: 0
  ptr:
    Mem:
      Int:
        children:
          - key: 0
            ptr:
              Mem:
                Int:
                  children:
                    - key: 0
                      ptr:
                        Mem:
                          Leaf:
                            items:
                              0: 0.0
                              1: 1.0
                    - key: 3
                      ptr:
                        Mem:
                          Leaf:
                            items:
                              3: 3.0
                              4: 4.0
                    - key: 6
                      ptr:
                        Mem:
                          Leaf:
                            items:
                              6: 6.0
                              7: 7.0
          - key: 9
            ptr:
              Mem:
                Int:
                  children:
                    - key: 9
                      ptr:
                        Mem:
                          Leaf:
                            items:
                              9: 9.0
                              10: 10.0
                    - key: 12
                      ptr:
                        Mem:
                          Leaf:
                            items:
                              12: 12.0
                              13: 13.0
                    - key: 15
                      ptr:
                        Mem:
                          Leaf:
                            items:
                              15: 15.0
                              16: 16.0
          - key: 18
            ptr:
              Mem:
                Int:
                  children:
                    - key: 18
                      ptr:
                        Mem:
                          Leaf:
                            items:
                              18: 18.0
                              19: 19.0
                    - key: 21
                      ptr:
                        Mem:
                          Leaf:
                            items:
                              21: 21.0
                              22: 22.0
                              23: 23.0"#);
    let r2 = current_thread::block_on_all(tree.remove(23));
    assert!(r2.is_ok());
    assert_eq!(format!("{}", &tree),
r#"---
height: 3
min_fanout: 2
max_fanout: 5
_max_size: 4194304
root:
  key: 0
  ptr:
    Mem:
      Int:
        children:
          - key: 0
            ptr:
              Mem:
                Int:
                  children:
                    - key: 0
                      ptr:
                        Mem:
                          Leaf:
                            items:
                              0: 0.0
                              1: 1.0
                    - key: 3
                      ptr:
                        Mem:
                          Leaf:
                            items:
                              3: 3.0
                              4: 4.0
                    - key: 6
                      ptr:
                        Mem:
                          Leaf:
                            items:
                              6: 6.0
                              7: 7.0
          - key: 9
            ptr:
              Mem:
                Int:
                  children:
                    - key: 9
                      ptr:
                        Mem:
                          Leaf:
                            items:
                              9: 9.0
                              10: 10.0
                    - key: 12
                      ptr:
                        Mem:
                          Leaf:
                            items:
                              12: 12.0
                              13: 13.0
                    - key: 15
                      ptr:
                        Mem:
                          Leaf:
                            items:
                              15: 15.0
                              16: 16.0
                    - key: 18
                      ptr:
                        Mem:
                          Leaf:
                            items:
                              18: 18.0
                              19: 19.0
                    - key: 21
                      ptr:
                        Mem:
                          Leaf:
                            items:
                              21: 21.0
                              22: 22.0"#);
}

#[test]
fn remove_and_merge_int_right() {
    let ddml = Box::new(Scenario::new().create_mock::<MockDDML>());
    let tree: Tree<u32, f32> = Tree::from_str(ddml, r#"
---
height: 3
min_fanout: 2
max_fanout: 5
_max_size: 4194304
root:
  key: 0
  ptr:
    Mem:
      Int:
        children:
          - key: 0
            ptr:
              Mem:
                Int:
                  children:
                    - key: 0
                      ptr:
                        Mem:
                          Leaf:
                            items:
                              0: 0.0
                              1: 1.0
                    - key: 2
                      ptr:
                        Mem:
                          Leaf:
                            items:
                              2: 2.0
                              3: 3.0
                              4: 4.0
          - key: 9
            ptr:
              Mem:
                Int:
                  children:
                    - key: 9
                      ptr:
                        Mem:
                          Leaf:
                            items:
                              9: 9.0
                              10: 10.0
                    - key: 12
                      ptr:
                        Mem:
                          Leaf:
                            items:
                              12: 12.0
                              13: 13.0
                    - key: 15
                      ptr:
                        Mem:
                          Leaf:
                            items:
                              15: 15.0
                              16: 16.0
          - key: 18
            ptr:
              Mem:
                Int:
                  children:
                    - key: 18
                      ptr:
                        Mem:
                          Leaf:
                            items:
                              18: 18.0
                              19: 19.0
                    - key: 21
                      ptr:
                        Mem:
                          Leaf:
                            items:
                              21: 21.0
                              22: 22.0"#);
    let r2 = current_thread::block_on_all(tree.remove(4));
    assert!(r2.is_ok());
    println!("{}", &tree);
    assert_eq!(format!("{}", &tree),
r#"---
height: 3
min_fanout: 2
max_fanout: 5
_max_size: 4194304
root:
  key: 0
  ptr:
    Mem:
      Int:
        children:
          - key: 0
            ptr:
              Mem:
                Int:
                  children:
                    - key: 0
                      ptr:
                        Mem:
                          Leaf:
                            items:
                              0: 0.0
                              1: 1.0
                    - key: 2
                      ptr:
                        Mem:
                          Leaf:
                            items:
                              2: 2.0
                              3: 3.0
                    - key: 9
                      ptr:
                        Mem:
                          Leaf:
                            items:
                              9: 9.0
                              10: 10.0
                    - key: 12
                      ptr:
                        Mem:
                          Leaf:
                            items:
                              12: 12.0
                              13: 13.0
                    - key: 15
                      ptr:
                        Mem:
                          Leaf:
                            items:
                              15: 15.0
                              16: 16.0
          - key: 18
            ptr:
              Mem:
                Int:
                  children:
                    - key: 18
                      ptr:
                        Mem:
                          Leaf:
                            items:
                              18: 18.0
                              19: 19.0
                    - key: 21
                      ptr:
                        Mem:
                          Leaf:
                            items:
                              21: 21.0
                              22: 22.0"#);
}

#[test]
fn remove_and_merge_leaf_left() {
    let ddml = Box::new(Scenario::new().create_mock::<MockDDML>());
    let tree: Tree<u32, f32> = Tree::from_str(ddml, r#"
---
height: 2
min_fanout: 2
max_fanout: 5
_max_size: 4194304
root:
  key: 0
  ptr:
    Mem:
      Int:
        children:
          - key: 0
            ptr:
              Mem:
                Leaf:
                  items:
                    0: 0.0
                    1: 1.0
          - key: 3
            ptr:
              Mem:
                Leaf:
                  items:
                    3: 3.0
                    4: 4.0
          - key: 5
            ptr:
              Mem:
                Leaf:
                  items:
                    5: 5.0
                    7: 7.0
"#);
    let r2 = current_thread::block_on_all(tree.remove(7));
    assert!(r2.is_ok());
    assert_eq!(format!("{}", &tree),
r#"---
height: 2
min_fanout: 2
max_fanout: 5
_max_size: 4194304
root:
  key: 0
  ptr:
    Mem:
      Int:
        children:
          - key: 0
            ptr:
              Mem:
                Leaf:
                  items:
                    0: 0.0
                    1: 1.0
          - key: 3
            ptr:
              Mem:
                Leaf:
                  items:
                    3: 3.0
                    4: 4.0
                    5: 5.0"#);
}

#[test]
fn remove_and_merge_leaf_right() {
    let ddml = Box::new(Scenario::new().create_mock::<MockDDML>());
    let tree: Tree<u32, f32> = Tree::from_str(ddml, r#"
---
height: 2
min_fanout: 2
max_fanout: 5
_max_size: 4194304
root:
  key: 0
  ptr:
    Mem:
      Int:
        children:
          - key: 0
            ptr:
              Mem:
                Leaf:
                  items:
                    0: 0.0
                    1: 1.0
          - key: 3
            ptr:
              Mem:
                Leaf:
                  items:
                    3: 3.0
                    4: 4.0
          - key: 5
            ptr:
              Mem:
                Leaf:
                  items:
                    5: 5.0
                    6: 6.0
                    7: 7.0
"#);
    let r2 = current_thread::block_on_all(tree.remove(4));
    assert!(r2.is_ok());
    assert_eq!(format!("{}", &tree),
r#"---
height: 2
min_fanout: 2
max_fanout: 5
_max_size: 4194304
root:
  key: 0
  ptr:
    Mem:
      Int:
        children:
          - key: 0
            ptr:
              Mem:
                Leaf:
                  items:
                    0: 0.0
                    1: 1.0
          - key: 3
            ptr:
              Mem:
                Leaf:
                  items:
                    3: 3.0
                    5: 5.0
                    6: 6.0
                    7: 7.0"#);
}

#[test]
fn remove_and_steal_int_left() {
    let ddml = Box::new(Scenario::new().create_mock::<MockDDML>());
    let tree: Tree<u32, f32> = Tree::from_str(ddml, r#"
---
height: 3
min_fanout: 2
max_fanout: 5
_max_size: 4194304
root:
  key: 0
  ptr:
    Mem:
      Int:
        children:
          - key: 0
            ptr:
              Mem:
                Int:
                  children:
                    - key: 0
                      ptr:
                        Mem:
                          Leaf:
                            items:
                              0: 0.0
                              1: 1.0
                    - key: 2
                      ptr:
                        Mem:
                          Leaf:
                            items:
                              2: 2.0
                              3: 3.0
          - key: 9
            ptr:
              Mem:
                Int:
                  children:
                    - key: 9
                      ptr:
                        Mem:
                          Leaf:
                            items:
                              9: 9.0
                              10: 10.0
                    - key: 12
                      ptr:
                        Mem:
                          Leaf:
                            items:
                              12: 12.0
                              13: 13.0
                    - key: 15
                      ptr:
                        Mem:
                          Leaf:
                            items:
                              15: 15.0
                              16: 16.0
                    - key: 17
                      ptr:
                        Mem:
                          Leaf:
                            items:
                              17: 17.0
                              18: 18.0
                    - key: 19
                      ptr:
                        Mem:
                          Leaf:
                            items:
                              19: 19.0
                              20: 20.0
          - key: 21
            ptr:
              Mem:
                Int:
                  children:
                    - key: 21
                      ptr:
                        Mem:
                          Leaf:
                            items:
                              21: 21.0
                              22: 22.0
                    - key: 24
                      ptr:
                        Mem:
                          Leaf:
                            items:
                              24: 24.0
                              25: 25.0
                              26: 26.0"#);
    let r2 = current_thread::block_on_all(tree.remove(26));
    assert!(r2.is_ok());
    assert_eq!(format!("{}", &tree),
r#"---
height: 3
min_fanout: 2
max_fanout: 5
_max_size: 4194304
root:
  key: 0
  ptr:
    Mem:
      Int:
        children:
          - key: 0
            ptr:
              Mem:
                Int:
                  children:
                    - key: 0
                      ptr:
                        Mem:
                          Leaf:
                            items:
                              0: 0.0
                              1: 1.0
                    - key: 2
                      ptr:
                        Mem:
                          Leaf:
                            items:
                              2: 2.0
                              3: 3.0
          - key: 9
            ptr:
              Mem:
                Int:
                  children:
                    - key: 9
                      ptr:
                        Mem:
                          Leaf:
                            items:
                              9: 9.0
                              10: 10.0
                    - key: 12
                      ptr:
                        Mem:
                          Leaf:
                            items:
                              12: 12.0
                              13: 13.0
                    - key: 15
                      ptr:
                        Mem:
                          Leaf:
                            items:
                              15: 15.0
                              16: 16.0
                    - key: 17
                      ptr:
                        Mem:
                          Leaf:
                            items:
                              17: 17.0
                              18: 18.0
          - key: 19
            ptr:
              Mem:
                Int:
                  children:
                    - key: 19
                      ptr:
                        Mem:
                          Leaf:
                            items:
                              19: 19.0
                              20: 20.0
                    - key: 21
                      ptr:
                        Mem:
                          Leaf:
                            items:
                              21: 21.0
                              22: 22.0
                    - key: 24
                      ptr:
                        Mem:
                          Leaf:
                            items:
                              24: 24.0
                              25: 25.0"#);
}

#[test]
fn remove_and_steal_int_right() {
    let ddml = Box::new(Scenario::new().create_mock::<MockDDML>());
    let tree: Tree<u32, f32> = Tree::from_str(ddml, r#"
---
height: 3
min_fanout: 2
max_fanout: 5
_max_size: 4194304
root:
  key: 0
  ptr:
    Mem:
      Int:
        children:
          - key: 0
            ptr:
              Mem:
                Int:
                  children:
                    - key: 0
                      ptr:
                        Mem:
                          Leaf:
                            items:
                              0: 0.0
                              1: 1.0
                    - key: 2
                      ptr:
                        Mem:
                          Leaf:
                            items:
                              2: 2.0
                              3: 3.0
          - key: 9
            ptr:
              Mem:
                Int:
                  children:
                    - key: 9
                      ptr:
                        Mem:
                          Leaf:
                            items:
                              9: 9.0
                              10: 10.0
                    - key: 12
                      ptr:
                        Mem:
                          Leaf:
                            items:
                              12: 12.0
                              13: 13.0
                              14: 14.0
          - key: 15
            ptr:
              Mem:
                Int:
                  children:
                    - key: 15
                      ptr:
                        Mem:
                          Leaf:
                            items:
                              15: 15.0
                              16: 16.0
                    - key: 17
                      ptr:
                        Mem:
                          Leaf:
                            items:
                              17: 17.0
                              18: 18.0
                    - key: 19
                      ptr:
                        Mem:
                          Leaf:
                            items:
                              19: 19.0
                              20: 20.0
                    - key: 21
                      ptr:
                        Mem:
                          Leaf:
                            items:
                              21: 21.0
                              22: 22.0
                    - key: 24
                      ptr:
                        Mem:
                          Leaf:
                            items:
                              24: 24.0
                              26: 26.0"#);
    let r2 = current_thread::block_on_all(tree.remove(14));
    assert!(r2.is_ok());
    assert_eq!(format!("{}", &tree),
r#"---
height: 3
min_fanout: 2
max_fanout: 5
_max_size: 4194304
root:
  key: 0
  ptr:
    Mem:
      Int:
        children:
          - key: 0
            ptr:
              Mem:
                Int:
                  children:
                    - key: 0
                      ptr:
                        Mem:
                          Leaf:
                            items:
                              0: 0.0
                              1: 1.0
                    - key: 2
                      ptr:
                        Mem:
                          Leaf:
                            items:
                              2: 2.0
                              3: 3.0
          - key: 9
            ptr:
              Mem:
                Int:
                  children:
                    - key: 9
                      ptr:
                        Mem:
                          Leaf:
                            items:
                              9: 9.0
                              10: 10.0
                    - key: 12
                      ptr:
                        Mem:
                          Leaf:
                            items:
                              12: 12.0
                              13: 13.0
                    - key: 15
                      ptr:
                        Mem:
                          Leaf:
                            items:
                              15: 15.0
                              16: 16.0
          - key: 17
            ptr:
              Mem:
                Int:
                  children:
                    - key: 17
                      ptr:
                        Mem:
                          Leaf:
                            items:
                              17: 17.0
                              18: 18.0
                    - key: 19
                      ptr:
                        Mem:
                          Leaf:
                            items:
                              19: 19.0
                              20: 20.0
                    - key: 21
                      ptr:
                        Mem:
                          Leaf:
                            items:
                              21: 21.0
                              22: 22.0
                    - key: 24
                      ptr:
                        Mem:
                          Leaf:
                            items:
                              24: 24.0
                              26: 26.0"#);
}

#[test]
fn remove_and_steal_leaf_left() {
    let ddml = Box::new(Scenario::new().create_mock::<MockDDML>());
    let tree: Tree<u32, f32> = Tree::from_str(ddml, r#"
---
height: 2
min_fanout: 2
max_fanout: 5
_max_size: 4194304
root:
  key: 0
  ptr:
    Mem:
      Int:
        children:
          - key: 0
            ptr:
              Mem:
                Leaf:
                  items:
                    0: 0.0
                    1: 1.0
          - key: 2
            ptr:
              Mem:
                Leaf:
                  items:
                    2: 2.0
                    3: 3.0
                    4: 4.0
                    5: 5.0
                    6: 6.0
          - key: 8
            ptr:
              Mem:
                Leaf:
                  items:
                    8: 8.0
                    9: 9.0
"#);
    let r2 = current_thread::block_on_all(tree.remove(8));
    assert!(r2.is_ok());
    assert_eq!(format!("{}", &tree),
r#"---
height: 2
min_fanout: 2
max_fanout: 5
_max_size: 4194304
root:
  key: 0
  ptr:
    Mem:
      Int:
        children:
          - key: 0
            ptr:
              Mem:
                Leaf:
                  items:
                    0: 0.0
                    1: 1.0
          - key: 2
            ptr:
              Mem:
                Leaf:
                  items:
                    2: 2.0
                    3: 3.0
                    4: 4.0
                    5: 5.0
          - key: 6
            ptr:
              Mem:
                Leaf:
                  items:
                    6: 6.0
                    9: 9.0"#);
}

#[test]
fn remove_and_steal_leaf_right() {
    let ddml = Box::new(Scenario::new().create_mock::<MockDDML>());
    let tree: Tree<u32, f32> = Tree::from_str(ddml, r#"
---
height: 2
min_fanout: 2
max_fanout: 5
_max_size: 4194304
root:
  key: 0
  ptr:
    Mem:
      Int:
        children:
          - key: 0
            ptr:
              Mem:
                Leaf:
                  items:
                    0: 0.0
                    1: 1.0
          - key: 3
            ptr:
              Mem:
                Leaf:
                  items:
                    3: 3.0
                    4: 4.0
          - key: 5
            ptr:
              Mem:
                Leaf:
                  items:
                    5: 5.0
                    6: 6.0
                    7: 7.0
                    8: 8.0
                    9: 9.0
"#);
    let r2 = current_thread::block_on_all(tree.remove(4));
    assert!(r2.is_ok());
    assert_eq!(format!("{}", &tree),
r#"---
height: 2
min_fanout: 2
max_fanout: 5
_max_size: 4194304
root:
  key: 0
  ptr:
    Mem:
      Int:
        children:
          - key: 0
            ptr:
              Mem:
                Leaf:
                  items:
                    0: 0.0
                    1: 1.0
          - key: 3
            ptr:
              Mem:
                Leaf:
                  items:
                    3: 3.0
                    5: 5.0
          - key: 6
            ptr:
              Mem:
                Leaf:
                  items:
                    6: 6.0
                    7: 7.0
                    8: 8.0
                    9: 9.0"#);
}

#[test]
fn remove_nonexistent() {
    let ddml = Box::new(Scenario::new().create_mock::<MockDDML>());
    let tree: Tree<u32, f32> = Tree::new(ddml, 2, 5, 1<<22);
    let r = current_thread::block_on_all(tree.remove(3));
    assert_eq!(r, Ok(None));
}

}