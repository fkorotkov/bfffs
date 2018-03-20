// vim: tw=80

use futures::Future;
use futures::sync::oneshot;
use nix;
use std::cell::RefCell;
use std::cmp::{Ord, Ordering, PartialOrd};
use std::collections::BinaryHeap;
use std::rc::{Rc, Weak};
use tokio::executor::current_thread;
use tokio::reactor::Handle;

use common::*;
use common::dva::*;
use common::vdev::*;
use common::vdev_leaf::*;

struct BlockOpBufG<T> {
    pub buf: T,
    /// Used by the `VdevLeaf` to complete this future
    pub sender: oneshot::Sender<()>
}

enum BlockOpBufT {
    IoVec(BlockOpBufG<IoVec>),
    IoVecMut(BlockOpBufG<IoVecMut>),
    SGList(BlockOpBufG<SGList>),
    SGListMut(BlockOpBufG<SGListMut>)
}

/// A single read or write command that is queued at the `VdevBlock` layer
struct BlockOp {
    pub lba: LbaT,
    pub bufs: BlockOpBufT,
    /// The priority is the opposite of the distance from the scheduler's LBA at
    /// the time of `BlockOp` creation to the `BlockOp`'s LBA.  We use the
    /// opposite of distance because Rust's standard library includes a max heap
    /// but not a min heap.
    priority: LbaT
}

impl BlockOp {
    pub fn len(&self) -> usize {
        match self.bufs {
            BlockOpBufT::IoVec(ref iovec) => iovec.buf.len(),
            BlockOpBufT::IoVecMut(ref iovec) => iovec.buf.len(),
            BlockOpBufT::SGList(ref sglist) => {
                sglist.buf.iter().fold(0, |acc, iovec| acc + iovec.len())
            }
            BlockOpBufT::SGListMut(ref sglist) => {
                sglist.buf.iter().fold(0, |acc, iovec| acc + iovec.len())
            }
        }
    }
}

impl Eq for BlockOp {
}

impl Ord for BlockOp {
    /// Compare `BlockOp`s by priority.
    ///
    /// The priority is determined by the op's LBA compared to the scheduler's
    /// LBA *when the `BlockOp` is created*.
    fn cmp(&self, other: &BlockOp) -> Ordering {
        self.priority.cmp(&other.priority)
    }
}

impl PartialEq for BlockOp {
    fn eq(&self, other: &BlockOp) -> bool {
        self.priority == other.priority
    }
}

impl PartialOrd for BlockOp {
    fn partial_cmp(&self, other: &BlockOp) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl BlockOp {
    pub fn read_at(buf: IoVecMut, lba: LbaT, priority: LbaT,
                   sender: oneshot::Sender<()>) -> BlockOp {
        let g = BlockOpBufG::<IoVecMut>{ buf, sender };
        BlockOp { lba, bufs: BlockOpBufT::IoVecMut(g), priority: priority}
    }

    pub fn readv_at(bufs: SGListMut, lba: LbaT, priority: LbaT,
                    sender: oneshot::Sender<()>) -> BlockOp {
        let g = BlockOpBufG::<SGListMut>{buf: bufs, sender};
        BlockOp { lba, bufs: BlockOpBufT::SGListMut(g), priority: priority}
    }

    pub fn write_at(buf: IoVec, lba: LbaT, priority: LbaT,
                    sender: oneshot::Sender<()>) -> BlockOp {
        let g = BlockOpBufG::<IoVec>{ buf, sender };
        BlockOp { lba: lba, bufs: BlockOpBufT::IoVec(g), priority: priority}
    }

    pub fn writev_at(bufs: SGList, lba: LbaT, priority: LbaT,
                     sender: oneshot::Sender<()>) -> BlockOp {
        let g = BlockOpBufG::<SGList>{buf: bufs, sender};
        BlockOp { lba, bufs: BlockOpBufT::SGList(g), priority: priority}
    }
}

/// Future representing an any operation on a block vdev.
///
/// Since the scheduler combines adjacent operations, it's not always possible
/// to know how much of an original operation's data was successfully transacted
/// (as opposed to the combined operation's), so the return value is merely `()`
/// on success, or an error code on failure.
#[must_use = "futures do nothing unless polled"]
pub type VdevBlockFut = Future<Item = (), Error = nix::Error>;

struct Inner {
    /// Current queue depth
    queue_depth: usize,

    /// Underlying device
    pub leaf: Box<VdevLeaf>,

    /// The last LBA issued an operation
    last_lba: LbaT,

    /// A collection of BlockOps.  Newly received operations must land here.
    /// They will be issued to the OS as the scheduler sees fit.
    queue: BinaryHeap<BlockOp>,

    /// A `Weak` pointer back to `self`.  Used for closures that require a
    /// reference to `self`, but also require `'static` lifetime.
    weakself: Weak<RefCell<Inner>>
}

/// Helper macro used by Inner::issue_one
macro_rules! leaf_op {
    ( $buf:expr, $self:ident, $lba:expr, $weakself:ident, $func:ident) => {
        {
            let sender = $buf.sender;
            current_thread::spawn(
                $self.leaf.$func($buf.buf, $lba)
                .and_then( move |_| {
                    sender.send(()).unwrap();
                    let inner = $weakself.upgrade().expect(
                        "VdevBlock dropped with outstanding I/O");
                    inner.borrow_mut().queue_depth -= 1;
                    inner.borrow_mut().issue_all();
                    Ok(())
                })
                .map_err(|_| {
                    ()
                })
            )
        }
    }
}

//macro_rules! leaf_continuation {
    //( $sender:ident, $weakself:ident) => {
        //move |_| {
            //$sender.send(()).unwrap();
            //let inner = $weakself.upgrade().expect(
                //"VdevBlock dropped with outstanding I/O");
            //inner.borrow_mut().queue_depth -= 1;
            //inner.borrow_mut().issue_all();
            //Ok(())
        //}
    //}
//}

impl Inner {
    /// Maximum queue depth.  The value `10` is unscientifically chosen, and
    /// different values may be optimal for different drive types.
    const MAX_QUEUE_DEPTH: usize = 10;

    /// Issue as many scheduled operations as possible
    // Use the C-LOOK scheduling algorithm.  It guarantees that writes scheduled
    // in LBA order will also be issued in LBA order.
    fn issue_all(&mut self) {
        while self.queue_depth < Inner::MAX_QUEUE_DEPTH {
            let op = match self.queue.pop() {
                Some(op) => op,
                None => break
            };
            self.issue_one(op);
            // TODO: handle EAGAIN
        }
        // Ran out of operations to issue or exceeded queue depth.  If queue
        // depth was exceeded, an operation's completion will call issue_all
        // again.
    }

    /// Immediately issue one I/O operation
    fn issue_one(&mut self, block_op: BlockOp) {
        self.last_lba = block_op.lba;
        self.queue_depth += 1;
        let weakself = self.weakself.clone();

        // In the context where this is called, we can't return a future.  So we
        // have to spawn it into the event loop manually
        match block_op.bufs {
            BlockOpBufT::IoVec(iovec) =>
                leaf_op!(iovec, self, block_op.lba, weakself, write_at) ,
            BlockOpBufT::IoVecMut(iovec_mut) =>
                leaf_op!(iovec_mut, self, block_op.lba, weakself, read_at) ,
            BlockOpBufT::SGList(sglist) =>
                leaf_op!(sglist, self, block_op.lba, weakself, writev_at) ,
            BlockOpBufT::SGListMut(sglist_mut) =>
                leaf_op!(sglist_mut, self, block_op.lba, weakself, readv_at) ,
        };
    }

    /// Schedule the `block_op`, and possibly issue it too
    fn sched(&mut self, block_op: BlockOp) {
        self.queue.push(block_op);
        self.issue_all();
    }
}

/// `VdevBlock`: Virtual Device for basic block device
///
/// This struct contains the functionality that is common between all types of
/// leaf vdev.
pub struct VdevBlock {
    inner: Rc<RefCell<Inner>>,

    /// Handle to a Tokio reactor
    handle: Handle,

    /// Usable size of the vdev, in LBAs
    size:   LbaT,
}

impl VdevBlock {
    /// Helper function for read and write methods
    fn check_iovec_bounds(&self, lba: LbaT, buf: &[u8]) {
        let buflen = buf.len() as u64;
        let last_lba : LbaT = lba + buflen / (dva::BYTES_PER_LBA as u64);
        assert!(last_lba < self.size as u64)
    }

    /// Helper function for read and write methods
    fn check_sglist_bounds(&self, lba: LbaT, bufs: &[IoVec]) {
        let len : u64 = bufs.iter().fold(0, |accumulator, buf| {
            accumulator + buf.len() as u64
        });
        assert!(lba + len / (dva::BYTES_PER_LBA as u64) < self.size as u64)
    }

    /// Helper function for readv and writev methods
    ///
    /// TODO: combine this method with `check_sglist_bounds`
    fn check_sglistmut_bounds(&self, lba: LbaT, bufs: &[IoVecMut]) {
        let len : u64 = bufs.iter().fold(0, |accumulator, buf| {
            accumulator + buf.len() as u64
        });
        assert!(lba + len / (dva::BYTES_PER_LBA as u64) < self.size as u64)
    }

    /// Open a VdevBlock
    ///
    /// * `leaf`    An already-open underlying VdevLeaf 
    // The 'static enforces that if the VdevLeaf implementor contains any
    // references, they must be 'static.
    pub fn open<T: VdevLeaf + 'static>(leaf: Box<T>, handle: Handle) -> Self {
        let size = leaf.size();
        let inner = Rc::new(RefCell::new(Inner {
            queue_depth: 0,
            leaf,
            last_lba: 0,
            queue: BinaryHeap::new(),
            weakself: Weak::new()
        }));
        inner.borrow_mut().weakself = Rc::downgrade(&inner);
        VdevBlock {
            inner,
            handle,
            size,
        }
    }

    /// Compute the current scheduling priority of the given LBA.
    ///
    /// Though `self` may change, the computed priority will remain valid.
    fn priority(&self, lba: LbaT) -> LbaT {
        self.inner.borrow().last_lba.wrapping_sub(lba + 1)
    }

    /// Asynchronously read a contiguous portion of the vdev.
    ///
    /// Return the number of bytes actually read.
    pub fn read_at(&self, buf: IoVecMut, lba: LbaT) -> Box<VdevBlockFut> {
        self.check_iovec_bounds(lba, &buf);
        let (sender, receiver) = oneshot::channel::<()>();
        let priority = self.priority(lba);
        let block_op = BlockOp::read_at(buf, lba, priority, sender);
        self.inner.borrow_mut().sched(block_op);
        Box::new(receiver.map_err(|_| nix::Error::from(nix::errno::Errno::EPIPE)))
    }

    /// The asynchronous scatter/gather read function.
    ///
    /// Returns nothing on success, and on error on failure
    ///
    /// # Parameters
    ///
    /// * `bufs`	Scatter-gather list of buffers to receive data
    /// * `lba`     LBA from which to read
    pub fn readv_at(&self, bufs: SGListMut, lba: LbaT) -> Box<VdevBlockFut> {
        self.check_sglistmut_bounds(lba, &bufs);
        let (sender, receiver) = oneshot::channel::<()>();
        let priority = self.priority(lba);
        let block_op = BlockOp::readv_at(bufs, lba, priority, sender);
        self.inner.borrow_mut().sched(block_op);
        Box::new(receiver.map_err(|_| nix::Error::from(nix::errno::Errno::EPIPE)))
    }

    /// Asynchronously write a contiguous portion of the vdev.
    ///
    /// Returns nothing on success, and on error on failure
    pub fn write_at(&self, buf: IoVec, lba: LbaT) -> Box<VdevBlockFut> {
        self.check_iovec_bounds(lba, &buf);
        let (sender, receiver) = oneshot::channel::<()>();
        let priority = self.priority(lba);
        let block_op = BlockOp::write_at(buf, lba, priority, sender);
        assert_eq!(block_op.len() % BYTES_PER_LBA, 0,
            "VdevBlock does not support fragmentary writes");
        self.inner.borrow_mut().sched(block_op);
        Box::new(receiver.map_err(|_| nix::Error::from(nix::errno::Errno::EPIPE)))
    }

    /// The asynchronous scatter/gather write function.
    ///
    /// Returns nothing on success, or an error on failure
    ///
    /// # Parameters
    ///
    /// * `bufs`	Scatter-gather list of buffers to receive data
    /// * `lba`     LBA from which to read
    pub fn writev_at(&mut self, bufs: SGList, lba: LbaT) -> Box<VdevBlockFut> {
        self.check_sglist_bounds(lba, &bufs);
        let (sender, receiver) = oneshot::channel::<()>();
        let priority = self.priority(lba);
        let block_op = BlockOp::writev_at(bufs, lba, priority, sender);
        assert_eq!(block_op.len() % BYTES_PER_LBA, 0,
            "VdevBlock does not support fragmentary writes");
        self.inner.borrow_mut().sched(block_op);
        Box::new(receiver.map_err(|_| nix::Error::from(nix::errno::Errno::EPIPE)))
    }
}

impl Vdev for VdevBlock {
    fn handle(&self) -> Handle {
        self.handle.clone()
    }

    fn lba2zone(&self, lba: LbaT) -> Option<ZoneT> {
        self.inner.borrow().leaf.lba2zone(lba)
    }

    fn size(&self) -> LbaT {
        self.size
    }

    fn zone_limits(&self, zone: ZoneT) -> (LbaT, LbaT) {
        self.inner.borrow().leaf.zone_limits(zone)
    }
}

#[cfg(feature = "mocks")]
#[cfg(test)]
test_suite! {
    name mock_vdev_block;

    use super::*;
    use divbuf::DivBufShared;
    use futures::future;
    use mockers::{Scenario, Sequence};
    use mockers::matchers::ANY;
    use tokio::executor::current_thread;
    use tokio::reactor::Handle;

    mock!{
        MockVdevLeaf2,
        vdev,
        trait Vdev {
            fn handle(&self) -> Handle;
            fn lba2zone(&self, lba: LbaT) -> Option<ZoneT>;
            fn size(&self) -> LbaT;
            fn zone_limits(&self, zone: ZoneT) -> (LbaT, LbaT);
        },
        vdev_leaf,
        trait VdevLeaf  {
            fn read_at(&self, buf: IoVecMut, lba: LbaT) -> Box<IoVecFut>;
            fn readv_at(&self, bufs: SGListMut, lba: LbaT) -> Box<SGListFut>;
            fn write_at(&mut self, buf: IoVec, lba: LbaT) -> Box<IoVecFut>;
            fn writev_at(&mut self, bufs: SGList, lba: LbaT) -> Box<SGListFut>;
        }
    }

    fixture!( mocks() -> (Scenario, Box<MockVdevLeaf2>) {
            setup(&mut self) {
            let scenario = Scenario::new();
            let leaf = Box::new(scenario.create_mock::<MockVdevLeaf2>());
            scenario.expect(leaf.size_call()
                                .and_return(16384));
            scenario.expect(leaf.lba2zone_call(ANY)
                                .and_return_clone(Some(0))
                                .times(..));
            scenario.expect(leaf.zone_limits_call(0)
                                .and_return_clone((0, 1 << 19))
                                .times(..));
            (scenario, leaf)
        }
    });

    // basic reading works
    test read_at(mocks) {
        let scenario = mocks.val.0;
        let leaf = mocks.val.1;
        let mut seq = Sequence::new();
        let r0 = IoVecResult { value: 4096 };
        seq.expect(leaf.read_at_call(ANY, 1)
                       .and_return(Box::new(future::ok::<IoVecResult,
                                                         nix::Error>(r0))));
        scenario.expect(seq);

        let dbs0 = DivBufShared::from(vec![0u8; 4096]);
        let rbuf0 = dbs0.try_mut().unwrap();
        let vdev = VdevBlock::open(leaf, Handle::current());
        current_thread::block_on_all(future::lazy(|| {
            vdev.read_at(rbuf0, 1)
        })).unwrap();
    }

    // vectored reading works
    test readv_at(mocks) {
        let scenario = mocks.val.0;
        let leaf = mocks.val.1;
        let mut seq = Sequence::new();
        let r0 = SGListResult { value: 4096 };
        seq.expect(leaf.readv_at_call(ANY, 1)
                       .and_return(Box::new(future::ok::<SGListResult,
                                                         nix::Error>(r0))));
        scenario.expect(seq);

        let dbs0 = DivBufShared::from(vec![0u8; 4096]);
        let rbuf0 = vec![dbs0.try_mut().unwrap()];
        let vdev = VdevBlock::open(leaf, Handle::current());
        current_thread::block_on_all(future::lazy(|| {
            vdev.readv_at(rbuf0, 1)
        })).unwrap();
    }

    // Queued operations will both complete
    test queued(mocks) {
        let scenario = mocks.val.0;
        let leaf = mocks.val.1;
        let r0 = IoVecResult { value: 4096 };
        let r1 = IoVecResult { value: 4096 };
        let (sender, receiver) = oneshot::channel::<()>();
        let e = nix::Error::from(nix::errno::Errno::EPIPE);
        let fut0 = receiver.map(move |_| r0).map_err(move |_| e);
        let fut1 = future::ok::<IoVecResult, nix::Error>(r1);
        scenario.expect(leaf.read_at_call(ANY, 0)
                            .and_return(Box::new(fut0)));
        scenario.expect(leaf.read_at_call(ANY, 1)
                            .and_call(|_, _| {
                                sender.send(()).unwrap();
                                Box::new(fut1)
                            }));
        let dbs0 = DivBufShared::from(vec![0u8; 4096]);
        let dbs1 = DivBufShared::from(vec![0u8; 4096]);
        let rbuf0 = dbs0.try_mut().unwrap();
        let rbuf1 = dbs1.try_mut().unwrap();
        let vdev = VdevBlock::open(leaf, Handle::current());
        current_thread::block_on_all(future::lazy(|| {
            let f0 = vdev.read_at(rbuf0, 0);
            let f1 = vdev.read_at(rbuf1, 1);
            f0.join(f1)
        })).unwrap();
    }

    // Operations will be buffered after the max queue depth is reached
    // The first MAX_QUEUE_DEPTH operations will be issued immediately, in the
    // order in which they are requested.  Subsequent operations will be
    // reordered into LBA order
    test queue_depth(mocks) {
        let scenario = mocks.val.0;
        let leaf = mocks.val.1;
        let num_ops = Inner::MAX_QUEUE_DEPTH + 2;
        let mut seq = Sequence::new();

        let channels = (0..num_ops - 2).map(|_| oneshot::channel::<()>());
        let (futs, senders) : (Vec<_>, Vec<_>) = channels.map(|chan| {
            let e = nix::Error::from(nix::errno::Errno::EPIPE);
            (chan.1.map(|_| IoVecResult{value: 4096}).map_err(move |_| e),
             chan.0)
        })
        .unzip();
        for (i, f) in futs.into_iter().enumerate().rev() {
            seq.expect(leaf.write_at_call(ANY, i as LbaT)
                           .and_return(Box::new(f)));
        }
        // Schedule the final two operations in reverse LBA order, but verify
        // that they get issued in actual LBA order
        let final_result = IoVecResult {value: 4096};
        let final_fut = future::ok::<IoVecResult, nix::Error>(final_result);
        seq.expect(leaf.write_at_call(ANY, num_ops as LbaT - 2)
                            .and_call(|_, _| {
                                Box::new(final_fut)
                            }));
        let penultimate_result = IoVecResult {value: 4096};
        let penultimate_fut = future::ok::<IoVecResult,
                                           nix::Error>(penultimate_result);
        seq.expect(leaf.write_at_call(ANY, num_ops as LbaT - 1)
                            .and_call(|_, _| {
                                Box::new(penultimate_fut)
                            }));
        scenario.expect(seq);
        let dbs = DivBufShared::from(vec![0u8; 4096]);
        let wbuf = dbs.try().unwrap();
        let vdev = VdevBlock::open(leaf, Handle::current());
        current_thread::block_on_all(future::lazy(|| {
            // First schedule all operations.  There are too many to issue them
            // all immediately
            let unbuf_fut = future::join_all((0..num_ops - 2).rev().map(|i| {
                vdev.write_at(wbuf.clone(), i as LbaT)
            }));
            let penultimate_fut = vdev.write_at(wbuf.clone(),
                                                (num_ops - 1) as LbaT);
            let final_fut = vdev.write_at(wbuf.clone(),
                                          (num_ops - 2) as LbaT);
            let fut = unbuf_fut.join3(penultimate_fut, final_fut);
            // Verify that they weren't all issued
            assert_eq!(vdev.inner.borrow_mut().queue.len(), 2);
            // Finally, complete them.
            for chan in senders {
                chan.send(()).unwrap();
            }
            fut
        })).unwrap();
    }

    // Basic writing works
    test write_at(mocks) {
        let scenario = mocks.val.0;
        let leaf = mocks.val.1;
        let r = IoVecResult { value: 4096 };
        scenario.expect(leaf.write_at_call(ANY, 0)
                            .and_return(Box::new(future::ok::<IoVecResult,
                                                              nix::Error>(r))));

        let dbs = DivBufShared::from(vec![0u8; 4096]);
        let wbuf = dbs.try().unwrap();
        let vdev = VdevBlock::open(leaf, Handle::current());
        current_thread::block_on_all(future::lazy(|| {
            vdev.write_at(wbuf, 0)
        })).unwrap();
    }

    // vectored writing works
    test writev_at(mocks) {
        let scenario = mocks.val.0;
        let leaf = mocks.val.1;
        let r = SGListResult { value: 4096 };
        scenario.expect(leaf.writev_at_call(ANY, 0)
                            .and_return(Box::new(future::ok::<SGListResult,
                                                              nix::Error>(r))));

        let dbs = DivBufShared::from(vec![0u8; 4096]);
        let wbuf = vec![dbs.try().unwrap()];
        let mut vdev = VdevBlock::open(leaf, Handle::current());
        current_thread::block_on_all(future::lazy(|| {
            vdev.writev_at(wbuf, 0)
        })).unwrap();
    }
}
