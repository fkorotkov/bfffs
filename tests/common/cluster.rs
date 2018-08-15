// vim: tw=80

macro_rules! t {
    ($e:expr) => (match $e {
        Ok(e) => e,
        Err(e) => panic!("{} failed with {:?}", stringify!($e), e),
    })
}

test_suite! {
    name persistence;

    use bfffs::common::label::*;
    use bfffs::common::vdev_block::*;
    use bfffs::common::vdev_raid::*;
    use bfffs::common::cluster::*;
    use bfffs::sys::vdev_file::*;
    use futures::{Future, future};
    use std::{
        fs,
        io::{Read, Seek, SeekFrom, Write},
        num::NonZeroU64
    };
    use tempdir::TempDir;
    use tokio::runtime::current_thread::Runtime;

    // To regenerate this literal, dump the binary label using this command:
    // hexdump -e '8/1 "0x%02x, " " // "' -e '8/1 "%_p" "\n"' /tmp/label.bin
    const GOLDEN_LABEL: [u8; 0x10e] = [
        0x42, 0x46, 0x46, 0x46, 0x53, 0x20, 0x56, 0x64, // BFFFS Vd
        0x65, 0x76, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // ev......
        0x7e, 0x7c, 0x71, 0x58, 0x9b, 0xc8, 0x66, 0xff, // ~|qX..f.
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xee, // ........
        0xa3, 0x64, 0x75, 0x75, 0x69, 0x64, 0x50, 0x28, // .duuidP(
        0x5b, 0xa5, 0x43, 0xeb, 0x5f, 0x48, 0x39, 0xaf, // [.C._H9.
        0x45, 0x72, 0x90, 0x68, 0xfa, 0x56, 0x1b, 0x6d, // Er.h.V.m
        0x6c, 0x62, 0x61, 0x73, 0x5f, 0x70, 0x65, 0x72, // lbas_per
        0x5f, 0x7a, 0x6f, 0x6e, 0x65, 0x1a, 0x00, 0x01, // _zone...
        0x00, 0x00, 0x64, 0x6c, 0x62, 0x61, 0x73, 0x1a, // ..dlbas.
        0x00, 0x02, 0x00, 0x00, 0xa6, 0x64, 0x75, 0x75, // .....duu
        0x69, 0x64, 0x50, 0x9a, 0x82, 0x93, 0xac, 0x17, // idP.....
        0x42, 0x43, 0xbb, 0x85, 0x16, 0x84, 0xd1, 0xe0, // BC......
        0xfd, 0xa3, 0xe7, 0x69, 0x63, 0x68, 0x75, 0x6e, // ...ichun
        0x6b, 0x73, 0x69, 0x7a, 0x65, 0x10, 0x70, 0x64, // ksize.pd
        0x69, 0x73, 0x6b, 0x73, 0x5f, 0x70, 0x65, 0x72, // isks_per
        0x5f, 0x73, 0x74, 0x72, 0x69, 0x70, 0x65, 0x01, // _stripe.
        0x6a, 0x72, 0x65, 0x64, 0x75, 0x6e, 0x64, 0x61, // jredunda
        0x6e, 0x63, 0x79, 0x00, 0x70, 0x6c, 0x61, 0x79, // ncy.play
        0x6f, 0x75, 0x74, 0x5f, 0x61, 0x6c, 0x67, 0x6f, // out_algo
        0x72, 0x69, 0x74, 0x68, 0x6d, 0x68, 0x4e, 0x75, // rithmhNu
        0x6c, 0x6c, 0x52, 0x61, 0x69, 0x64, 0x68, 0x63, // llRaidhc
        0x68, 0x69, 0x6c, 0x64, 0x72, 0x65, 0x6e, 0x81, // hildren.
        0x50, 0x28, 0x5b, 0xa5, 0x43, 0xeb, 0x5f, 0x48, // P([.C._H
        0x39, 0xaf, 0x45, 0x72, 0x90, 0x68, 0xfa, 0x56, // 9.Er.h.V
        0x1b,                                           // .
        // The Cluster portion of the label starts here
              0xa3, 0x70, 0x61, 0x6c, 0x6c, 0x6f, 0x63, //  .palloc
        0x61, 0x74, 0x65, 0x64, 0x5f, 0x62, 0x6c, 0x6f, // ated_blo
        0x63, 0x6b, 0x73, 0x82, 0x00, 0x00, 0x6c, 0x66, // cks...lf
        0x72, 0x65, 0x65, 0x64, 0x5f, 0x62, 0x6c, 0x6f, // reed_blo
        0x63, 0x6b, 0x73, 0x82, 0x00, 0x00, 0x64, 0x74, // cks...dt
        0x78, 0x67, 0x73, 0x82, 0xa2, 0x65, 0x73, 0x74, // xgs..est
        0x61, 0x72, 0x74, 0x00, 0x63, 0x65, 0x6e, 0x64, // art.cend
        0x00, 0xa2, 0x65, 0x73, 0x74, 0x61, 0x72, 0x74, // ..estart
        0x00, 0x63, 0x65, 0x6e, 0x64, 0x00,
    ];

    fixture!( objects() -> (Runtime, Cluster, TempDir, String) {
        setup(&mut self) {
            let len = 1 << 29;  // 512 MB
            let tempdir = t!(TempDir::new("test_cluster_persistence"));
            let fname = format!("{}/vdev", tempdir.path().display());
            let file = t!(fs::File::create(&fname));
            t!(file.set_len(len));
            let mut rt = Runtime::new().unwrap();
            let lpz = NonZeroU64::new(65536);
            let cluster = Cluster::create(16, 1, 1, lpz, 0, &[fname.clone()]);
            (rt, cluster, tempdir, fname)
        }
    });

    // Test Cluster::open
    test open(objects()) {
        {
            let mut f = fs::OpenOptions::new()
                .write(true)
                .open(objects.val.3.clone()).unwrap();
            f.write_all(&GOLDEN_LABEL).unwrap();
        }
        Runtime::new().unwrap().block_on(future::lazy(|| {
            VdevFile::open(objects.val.3.clone()).map(|(leaf, reader)| {
                (VdevBlock::new(leaf), reader)
            }).and_then(move |combined| {
                let (vdev_raid, reader) = VdevRaid::open(None, vec![combined]);
                 Cluster::open(vdev_raid, reader)
            }).map(|(cluster, _reader)| {
                assert_eq!(cluster.allocated(), 0);
            })
        })).unwrap();
    }

    test write_label(objects()) {
        let (mut rt, old_cluster, _tempdir, path) = objects.val;
        rt.block_on(future::lazy(|| {
            let label_writer = LabelWriter::new();
            old_cluster.write_label(label_writer)
        })).unwrap();

        let mut f = fs::File::open(path).unwrap();
        let mut v = vec![0; 8192];
        // Skip leaf and raid labels
        f.seek(SeekFrom::Start(201)).unwrap();
        f.read_exact(&mut v).unwrap();
        // Uncomment this block to save the binary label for inspection
        /* {
            use std::fs::File;
            use std::io::Write;
            let mut df = File::create("/tmp/label.bin").unwrap();
            df.write_all(&v[..]).unwrap();
        } */
        // Compare against the golden master
        assert_eq!(&v[0..69], &GOLDEN_LABEL[201..]);
        // Rest of the buffer should be zero-filled
        assert!(v[69..].iter().all(|&x| x == 0));
    }
}
