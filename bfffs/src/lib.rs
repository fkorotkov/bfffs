#![cfg_attr(feature = "nightly", feature(plugin))]
#![cfg_attr(all(feature = "nightly", test), feature(test))]

// Disable the range_plus_one lint until this bug is fixed.  It generates many
// false positive in the Tree code.
// https://github.com/rust-lang-nursery/rust-clippy/issues/3307
#![allow(clippy::range_plus_one)]

// I don't find this lint very helpful
#![allow(clippy::type_complexity)]

// I use a common pattern to substitute mock objects for real ones in test
// builds.  Silence clippy's complaints.
#![allow(clippy::module_inception)]

#[cfg(all(feature = "nightly", test))]
extern crate test;

pub mod common;

#[macro_export]
macro_rules! boxfut {
    ( $v:expr ) => {
        Box::new($v) as Box<dyn Future<Item=_, Error=_> + Send>
    };
    ( $v:expr, $e:ty ) => {
        Box::new($v) as Box<dyn Future<Item=_, Error=$e> + Send>
    };
    ( $v:expr, $i:ty, $e:ty ) => {
        Box::new($v) as Box<dyn Future<Item=$i, Error=$e> + Send>
    };
    ( $v:expr, $i:ty, $e:ty, $lt:lifetime ) => {
        Box::new($v) as Box<dyn Future<Item=$i, Error=$e> + $lt>
    };
}

#[macro_export]
macro_rules! boxstream {
    ( $v:expr ) => {
        Box::new($v) as Box<dyn Stream<Item=_, Error=_> + Send>
    };
}
