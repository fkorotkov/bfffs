// vim: tw=80

use common::*;
use futures;
use std::io;
use tokio::reactor::Handle;

/// Type returned by `IoVec`-oriented `Vdev` operations on success.
#[derive(Clone, Debug, Default)]
pub struct IoVecResult {
    /// The result that the operation would've had had it been synchronous.
    /// Usually the number of bytes read or written
    pub value: isize
}

/// Type returned by `SGList`-oriented `Vdev` operations on success.
#[derive(Clone, Debug, Default)]
pub struct SGListResult {
    /// The result that the operation would've had had it been synchronous.
    /// Usually the number of bytes read or written
    pub value: isize
}

/// Future representing an `IoVec`-oriented operation on a vdev.  The returned
/// value is the amount of data that was actually read/written, or an errno on
/// error.
pub type IoVecFut = futures::Future<Item = IoVecResult, Error = io::Error>;

/// Future representing an `SGList`-oriented operation on a vdev.  The returned
/// value is the amount of data that was actually read/written, or an errno on
/// error.
pub type SGListFut = futures::Future<Item = SGListResult, Error = io::Error>;

/// Vdev: Virtual Device
///
/// This is directly analogous to ZFS Vdevs.  A vdev is a virtual block device
/// built upon one or more virtual or real devices.  Unlike early version of
/// ZFS, vdevs may not be stacked arbitrarily.  Not all Vdevs have the same data
/// plane API.  These methods here are the methods that are common to all Vdev
/// types.
pub trait Vdev {
    /// Return the Tokio reactor handle used for this vdev
    fn handle(&self) -> Handle;

    /// Return the zone number at which the given LBA resides
    fn lba2zone(&self, lba: LbaT) -> ZoneT;

    /// Asynchronously read a contiguous portion of the vdev
    fn read_at(&self, buf: IoVecMut, lba: LbaT) -> Box<IoVecFut>;

    /// Return the usable space of the Vdev.
    ///
    /// Does not include wasted space, space reserved for labels, space used by
    /// parity, etc.  May not change within the lifetime of a Vdev.
    fn size(&self) -> LbaT;

    /// Return the first LBA of the given zone
    fn start_of_zone(&self, zone: ZoneT) -> LbaT;

    /// Asynchronously write a contiguous portion of the vdev
    fn write_at(&mut self, buf: IoVec, lba: LbaT) -> Box<IoVecFut>;
}

/// Scatter-Gather Vdev
///
/// These Vdevs support scatter-gather operations analogous to preadv/pwritev.
pub trait SGVdev : Vdev {
    /// The asynchronous scatter/gather read function.
    /// 
    /// * `bufs`	Scatter-gather list of buffers to receive data
    /// * `lba`     LBA from which to read
    fn readv_at(&self, bufs: SGListMut, lba: LbaT) -> Box<SGListFut>;

    /// The asynchronous scatter/gather write function.
    /// 
    /// * `bufs`	Scatter-gather list of buffers to receive data
    /// * `lba`     LBA from which to read
    fn writev_at(&mut self, bufs: SGList, lba: LbaT) -> Box<SGListFut>;
}
