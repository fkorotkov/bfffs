/// vim: tw=80

use common::*;
use std::cmp::{Ord, Ordering, PartialOrd};
use std::collections::BinaryHeap;
use std::collections::btree_map::BTreeMap;
use std::rc::Rc;
use tokio_file::{AioFut};

/// A single read or write command that is queued at the VdevBlock layer
#[derive(Eq)]
pub struct BlockOp {
    pub lba: LbaT,
    pub bufs: Box<[Rc<Box<[u8]>>]>
}

impl Ord for BlockOp {
    /// Compare `BlockOp`s by LBA in *reverse* order.  We must use reverse order
    /// because Rust's standard library includes a max heap but not a min heap,
    /// and we want to pop `BlockOp`s lowest-LBA first.
    fn cmp(&self, other: &BlockOp) -> Ordering {
        self.lba.cmp(&other.lba).reverse()
    }
}

impl PartialEq for BlockOp {
    fn eq(&self, other: &BlockOp) -> bool {
        self.lba == other.lba
    }
}

impl PartialOrd for BlockOp {
    fn partial_cmp(&self, other: &BlockOp) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// Used for scheduling writes within a single Zone
struct ZoneQueue {
    /// The zone's write pointer, as an LBA.  Absolute, not relative to
    /// start-of-zone.
    wp: LbaT,

    /// Priority queue of pending `BlockOp`s for a single zone.  It stores
    /// operations that aren't ready to be issued to the underlying storage,
    /// then issues them in LBA order.  There may be gaps between adjacent ops.
    /// However, sine it is illegal for the client to write to the same location
    /// twice without explicitly erasing the zone, there are guaranteed to be no
    /// overlapping ops.
    q: BinaryHeap<BlockOp>
}

/// ZoneScheduler: I/O scheduler for a single zoned block device
///
/// This object schedules I/O to a block device.  Its main purpose is to provide
/// the strictly sequential write operations that zoned devices require.
pub struct ZoneScheduler {
    /// A collection of ZoneQueues, one for each open Zone.  Newly received
    /// writes must land here.  They will be issued to the OS in LBA-order, per
    /// zone.  If a Zone is not present in the map, then it must be either full
    /// or empty.
    write_queues: BTreeMap<ZoneT, ZoneQueue>,

    /// A collection of BlockOps.  Newly received reads must land here.  They
    /// will be issued to the OS as the scheduler sees fit.
    read_queue: BTreeMap<LbaT, BlockOp>
}

impl ZoneScheduler {
    pub fn new() -> ZoneScheduler {
        ZoneScheduler{ write_queues: BTreeMap::new(),
                       read_queue: BTreeMap::new() }
    }

    pub fn read_at(&self, buf: Rc<Box<[u8]>>, lba: LbaT) -> (
        AioFut<isize>, ZoneSchedIter) {
        //TODO
        ZoneSchedIter{}
    }

    pub fn readv_at(&self, bufs: &[Rc<Box<[u8]>>], lba: LbaT) -> (
        AioFut<isize>, ZoneSchedIter) {
        //TODO
        ZoneSchedIter{}
    }

    pub fn write_at(&self, buf: Rc<Box<[u8]>>, lba: LbaT) -> (
        AioFut<isize>, ZoneSchedIter) {
        //TODO
        ZoneSchedIter{}
    }

    pub fn writev_at(&self, bufs: &[Rc<Box<[u8]>>], lba: LbaT) -> (
        AioFut<isize>, ZoneSchedIter) {
        //TODO
        ZoneSchedIter{}
    }
}

/// An iterator that yields successive `BlockOp`s of a `ZoneScheduler` that are
/// ready to issue
pub struct ZoneSchedIter {
}

impl Iterator for ZoneSchedIter {
    type Item = BlockOp;

    fn next(&mut self) -> Option<BlockOp> {
        //TODO
        None
    }
}
