// vim: tw=80

use std::rc::Rc;

pub mod block_fut;
pub mod dva;
pub mod vdev;
pub mod vdev_block;
pub mod vdev_leaf;
pub mod zone_scheduler;
pub mod zoned_device;
pub type LbaT = u64;
pub type ZoneT = u32;
/// Our IoVec.  Unlike the standard library's, ours is reference-counted so it
/// can have more than one owner.
pub type IoVec = Rc<Box<[u8]>>;
/// Our scatter-gather list.  A slice of reference-counted `IoVec`s
pub type SGList = Box<[Rc<Box<[u8]>>]>; 
