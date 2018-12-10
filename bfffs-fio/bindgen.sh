#! /bin/sh

# fio doesn't install the necessary headers, so we have to reference its source
# directory
FIOPATH="/usr/home/somers/freebsd/ports/head/benchmarks/fio/work/fio-3.12"

cat > src/ffi.rs << HERE
#![allow(non_camel_case_types)]
#![allow(non_upper_case_globals)]
#![allow(non_snake_case)]
#![allow(unused)]
use libc::timespec;
HERE

# Use 1.0 compatibility mode to workaround a rustfmt bug involving unions:
# https://github.com/rust-lang/rust-bindgen/issues/1120
bindgen --no-rustfmt-bindings \
	--no-layout-tests \
	--whitelist-function 'generic_.*_file' \
	--whitelist-type 'fio_file' \
	--whitelist-type 'fio_option' \
	--whitelist-type 'fio_opt_type.*' \
	--whitelist-type 'ioengine_ops' \
	--whitelist-type 'io_u' \
	--whitelist-type 'thread_data' \
	--whitelist-var 'FIO_IOOPS_VERSION' \
	--blacklist-type 'timespec' \
	--ctypes-prefix libc \
	--rust-target 1.0 \
	src/ffi.h -- -I$FIOPATH >> src/ffi.rs
rustfmt --force --write-mode replace src/ffi.rs
