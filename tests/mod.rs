#![cfg_attr(feature = "cargo-clippy", feature(tool_lints))]
// https://github.com/mindsbackyard/galvanic-test/pull/13
#![cfg_attr(feature = "cargo-clippy", allow(clippy::unnecessary_mut_passed))]


extern crate bfffs;
extern crate divbuf;
extern crate env_logger;
extern crate fixedbitset;
extern crate futures;
#[macro_use] extern crate galvanic_test;
extern crate itertools;
extern crate libc;
#[macro_use] extern crate log;
extern crate nix;
#[macro_use] extern crate pretty_assertions;
extern crate rand;
extern crate rand_xorshift;
extern crate tempdir;
extern crate tokio;
extern crate tokio_file;
extern crate tokio_io_pool;
extern crate uuid;

mod common;
mod sys;
