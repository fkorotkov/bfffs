#![cfg_attr(feature = "mocks", feature(plugin))]
#![cfg_attr(feature = "mocks", feature(proc_macro))]

extern crate atomic;
extern crate bincode;
#[macro_use]
extern crate bitfield;
extern crate blosc;
extern crate byteorder;
extern crate divbuf;
#[macro_use]
extern crate downcast;
#[macro_use]
extern crate enum_primitive_derive;
extern crate fixedbitset;
extern crate fuse;
extern crate futures;
extern crate futures_locks;
#[cfg(test)]
#[macro_use]
#[cfg(feature = "mocks")] extern crate galvanic_test;
extern crate itertools;
extern crate isa_l;
#[macro_use]
extern crate lazy_static;
extern crate libc;
extern crate metrohash;
#[cfg(test)]
#[macro_use]
#[cfg(feature = "mocks")] extern crate mockers;
#[cfg(feature = "mocks")] extern crate mockers_derive;
extern crate modulo;
extern crate num_traits;
#[cfg(test)]
extern crate permutohedron;
#[cfg(test)]
#[macro_use]
extern crate pretty_assertions;
extern crate nix;
#[cfg(test)]
extern crate rand;
extern crate serde;
extern crate serde_cbor;
#[macro_use]
extern crate serde_derive;
#[cfg(test)]
extern crate serde_yaml;
#[cfg(test)] extern crate simulacrum;
extern crate time;
extern crate tokio;
extern crate tokio_file;
extern crate tokio_io_pool;
extern crate uuid;

pub mod common;
pub mod sys;
