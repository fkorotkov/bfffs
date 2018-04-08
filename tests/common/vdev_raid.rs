// vim: tw=80

macro_rules! t {
    ($e:expr) => (match $e {
        Ok(e) => e,
        Err(e) => panic!("{} failed with {:?}", stringify!($e), e),
    })
}

test_suite! {
    // These tests use real VdevBlock and VdevLeaf objects
    name vdev_raid;

    use arkfs::common::*;
    use arkfs::common::vdev_raid::*;
    use arkfs::common::vdev::Vdev;
    use divbuf::DivBufShared;
    use futures::{Future, future};
    use rand::{Rng, thread_rng};
    use std::fs;
    use std::io::{Read, Seek, SeekFrom};
    use tempdir::TempDir;
    use tokio::executor::current_thread;
    use tokio::reactor::Handle;

    const GOLDEN_LABEL: [u8; 223] = [
        // First 16 bytes are file magic
        0x41, 0x72, 0x6b, 0x46, 0x53, 0x20, 0x56, 0x64,
        0x65, 0x76, 0x52, 0x61, 0x69, 0x64, 0x00, 0x00,
        // The rest is a serialized VdevRaid::Label object
        0xa2, 0x68, 0x63, 0x68, 0x65, 0x63, 0x6b, 0x73,
        0x75, 0x6d, 0x88,
        // These 8 bytes are a checksum
                          0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00,
        // This begins the serialized VdevRaid::LabelData object
                          0x64, 0x64, 0x61, 0x74, 0x61,
        0xa6, 0x64, 0x75, 0x75, 0x69, 0x64, 0x50,
        // These 16 bytes are the UUID
                                                  0xef,
        0x0b, 0x41, 0xf7, 0xd2, 0xd7, 0x47, 0x89, 0xa6,
        0xd1, 0x0b, 0xc1, 0x2a, 0xb7, 0x31, 0x8c,
        // Rest of the LabelData
                                                  0x69,
        0x63, 0x68, 0x75, 0x6e, 0x6b, 0x73, 0x69, 0x7a,
        0x65, 0x02, 0x70, 0x64, 0x69, 0x73, 0x6b, 0x73,
        0x5f, 0x70, 0x65, 0x72, 0x5f, 0x73, 0x74, 0x72,
        0x69, 0x70, 0x65, 0x03, 0x6a, 0x72, 0x65, 0x64,
        0x75, 0x6e, 0x64, 0x61, 0x6e, 0x63, 0x79, 0x01,
        0x70, 0x6c, 0x61, 0x79, 0x6f, 0x75, 0x74, 0x5f,
        0x61, 0x6c, 0x67, 0x6f, 0x72, 0x69, 0x74, 0x68,
        0x6d, 0x66, 0x50, 0x72, 0x69, 0x6d, 0x65, 0x53,
        0x68, 0x63, 0x68, 0x69, 0x6c, 0x64, 0x72, 0x65,
        0x6e, 0x85,
        // Array of child UUIDs
                    0x50, 0x78, 0x53, 0xdb, 0x9c, 0x6e,
        0xcf, 0x47, 0x75, 0x91, 0xd8, 0x13, 0x93, 0x13,
        0x06, 0x6b, 0x29, 0x50, 0x11, 0xad, 0x9d, 0x23,
        0x5b, 0xb7, 0x4d, 0x56, 0x86, 0x93, 0x49, 0x61,
        0x05, 0xe4, 0x8c, 0xf5, 0x50, 0x6c, 0x83, 0x93,
        0xc9, 0xfc, 0x5e, 0x47, 0xbd, 0xbd, 0x9f, 0x29,
        0x3b, 0xdf, 0xec, 0x3a, 0xbb, 0x50, 0x25, 0x21,
        0xad, 0x66, 0x47, 0x54, 0x4f, 0x2a, 0x9a, 0x87,
        0x68, 0xa7, 0x79, 0xf5, 0x3f, 0xc5, 0x50, 0x3a,
        0x22, 0x4a, 0x3e, 0xde, 0x72, 0x41, 0x5d, 0x8c,
        0x8e, 0x50, 0x79, 0x8d, 0xa4, 0x01, 0x9d
    ];

    fixture!( raid(n: i16, k: i16, f: i16, chunksize: LbaT) ->
              (VdevRaid, TempDir, Vec<String>) {

        params {
            vec![(3, 3, 1, 2),      // Smallest possible configuration
                 (5, 4, 1, 2),      // Smallest PRIMES declustered configuration
                 (5, 5, 2, 2),      // Smallest double-parity configuration
                 (7, 4, 1, 2),      // Smallest non-ideal PRIME-S configuration
                 (7, 7, 3, 2),      // Smallest triple-parity configuration
                 (11, 9, 4, 2),     // Smallest quad-parity configuration
            ].into_iter()
        }
        setup(&mut self) {

            let len = 1 << 30;  // 1 GB
            let tempdir = t!(TempDir::new("test_vdev_raid"));
            let paths = (0..*self.n).map(|i| {
                let fname = format!("{}/vdev.{}", tempdir.path().display(), i);
                let file = t!(fs::File::create(&fname));
                t!(file.set_len(len));
                fname
            }).collect::<Vec<_>>();
            let mut vdev_raid = VdevRaid::create(*self.chunksize,
                *self.n, *self.k, *self.f, LayoutAlgorithm::PrimeS, &paths,
                Handle::default());
            current_thread::block_on_all(
                vdev_raid.open_zone(0)
            ).expect("open_zone");
            (vdev_raid, tempdir, paths)
        }
    });

    fn make_bufs(chunksize: LbaT, k: i16, f: i16, s: usize) ->
        (DivBufShared, DivBufShared) {

        let chunks = s * (k - f) as usize;
        let lbas = chunksize * chunks as LbaT;
        let bytes = BYTES_PER_LBA * lbas as usize;
        let mut wvec = vec![0u8; bytes];
        let mut rng = thread_rng();
        for x in &mut wvec {
            *x = rng.gen();
        }
        let dbsw = DivBufShared::from(wvec);
        let dbsr = DivBufShared::from(vec![0u8; bytes]);
        (dbsw, dbsr)
    }

    fn write_read(vr: &VdevRaid, wbufs: Vec<IoVec>, rbufs: Vec<IoVecMut>,
                  zone: ZoneT, start_lba: LbaT) {
        let mut write_lba = start_lba;
        let mut read_lba = start_lba;
        current_thread::block_on_all(future::lazy(|| {
            future::join_all( {
                wbufs.into_iter()
                .map(|wb| {
                    let lbas = (wb.len() / BYTES_PER_LBA) as LbaT;
                    let fut = vr.write_at(wb, zone, write_lba);
                    write_lba += lbas;
                    fut
                })
            }).and_then(|_| {
                future::join_all({
                    rbufs.into_iter()
                    .map(|rb| {
                        let lbas = (rb.len() / BYTES_PER_LBA) as LbaT;
                        let fut = vr.read_at(rb, read_lba);
                        read_lba += lbas;
                        fut
                    })
                })
            })
        })).expect("current_thread::block_on_all");
    }

    fn write_read0(vr: VdevRaid, wbufs: Vec<IoVec>, rbufs: Vec<IoVecMut>) {
        let zl = vr.zone_limits(0);
        write_read(&vr, wbufs, rbufs, 0, zl.0)
    }

    fn write_read_n_stripes(vr: VdevRaid, chunksize: LbaT, k: i16, f: i16,
                            s: usize) {
        let (dbsw, dbsr) = make_bufs(chunksize, k, f, s);
        let wbuf0 = dbsw.try().unwrap();
        let wbuf1 = dbsw.try().unwrap();
        write_read0(vr, vec![wbuf1], vec![dbsr.try_mut().unwrap()]);
        assert_eq!(wbuf0, dbsr.try().unwrap());
    }

    fn writev_read_n_stripes(vr: &VdevRaid, chunksize: LbaT, k: i16, f: i16,
                             s: usize) {
        let zl = vr.zone_limits(0);
        let (dbsw, dbsr) = make_bufs(chunksize, k, f, s);
        let wbuf = dbsw.try().unwrap();
        let mut wbuf_l = wbuf.clone();
        let wbuf_r = wbuf_l.split_off(wbuf.len() / 2);
        let sglist = vec![wbuf_l, wbuf_r];
        current_thread::block_on_all(future::lazy(|| {
            vr.writev_at_one(&sglist, zl.0)
                .then(|write_result| {
                    write_result.expect("writev_at_one");
                    vr.read_at(dbsr.try_mut().unwrap(), zl.0)
                })
        })).expect("read_at");
        assert_eq!(wbuf, dbsr.try().unwrap());
    }

    // Read a stripe in several pieces, from disk
    test read_parts_of_stripe(raid((7, 7, 1, 16))) {
        let (dbsw, dbsr) = make_bufs(*raid.params.chunksize, *raid.params.k,
                                     *raid.params.f, 1);
        let cs = *raid.params.chunksize as usize;
        let wbuf = dbsw.try().unwrap();
        {
            let mut rbuf0 = dbsr.try_mut().unwrap();
            // rbuf0 will get the first part of the first chunk
            let mut rbuf1 = rbuf0.split_off(cs / 4 * BYTES_PER_LBA);
            // rbuf1 will get the middle of the first chunk
            let mut rbuf2 = rbuf1.split_off(cs / 2 * BYTES_PER_LBA);
            // rbuf2 will get the end of the first chunk
            let mut rbuf3 = rbuf2.split_off(cs / 4 * BYTES_PER_LBA);
            // rbuf3 will get an entire chunk
            let mut rbuf4 = rbuf3.split_off(cs * BYTES_PER_LBA);
            // rbuf4 will get 2 chunks
            let mut rbuf5 = rbuf4.split_off(2 * cs * BYTES_PER_LBA);
            // rbuf5 will get one and a half chunks
            // rbuf6 will get the last half chunk
            let rbuf6 = rbuf5.split_off(3 * cs / 2 * BYTES_PER_LBA);
            write_read0(raid.val.0, vec![wbuf.clone()],
                        vec![rbuf0, rbuf1, rbuf2, rbuf3, rbuf4, rbuf5, rbuf6]);
        }
        assert_eq!(&wbuf[..], &dbsr.try().unwrap()[..]);
    }

    // Read the end of one stripe and the beginning of another
    test read_partial_stripes(raid((3, 3, 1, 2))) {
        let (dbsw, dbsr) = make_bufs(*raid.params.chunksize, *raid.params.k,
                                     *raid.params.f, 2);
        let wbuf = dbsw.try().unwrap();
        {
            let mut rbuf_m = dbsr.try_mut().unwrap();
            let rbuf_b = rbuf_m.split_to(BYTES_PER_LBA);
            let l = rbuf_m.len();
            let rbuf_e = rbuf_m.split_off(l - BYTES_PER_LBA);
            write_read0(raid.val.0, vec![wbuf.clone()],
                        vec![rbuf_b, rbuf_m, rbuf_e]);
        }
        assert_eq!(wbuf, dbsr.try().unwrap());
    }

    #[should_panic]
    test read_past_end_of_stripe_buffer(raid((3, 3, 1, 2))) {
        let (dbsw, dbsr) = make_bufs(*raid.params.chunksize, *raid.params.k,
                                     *raid.params.f, 1);
        let wbuf = dbsw.try().unwrap();
        let wbuf_short = wbuf.slice_to(BYTES_PER_LBA);
        let rbuf = dbsr.try_mut().unwrap();
        write_read0(raid.val.0, vec![wbuf_short], vec![rbuf]);
    }

    #[should_panic]
    test read_starts_past_end_of_stripe_buffer(raid((3, 3, 1, 2))) {
        let (dbsw, dbsr) = make_bufs(*raid.params.chunksize, *raid.params.k,
                                     *raid.params.f, 1);
        let wbuf = dbsw.try().unwrap();
        let wbuf_short = wbuf.slice_to(BYTES_PER_LBA);
        let mut rbuf = dbsr.try_mut().unwrap();
        let rbuf_r = rbuf.split_off(BYTES_PER_LBA);
        write_read0(raid.val.0, vec![wbuf_short], vec![rbuf_r]);
    }

    test write_read_one_stripe(raid) {
        write_read_n_stripes(raid.val.0, *raid.params.chunksize,
                             *raid.params.k, *raid.params.f, 1);
    }

    // read_at_one/write_at_one with a large configuration
    test write_read_one_stripe_jumbo(raid((41, 19, 3, 2))) {
        write_read_n_stripes(raid.val.0, *raid.params.chunksize,
                             *raid.params.k, *raid.params.f, 1);
    }

    test write_read_two_stripes(raid) {
        write_read_n_stripes(raid.val.0, *raid.params.chunksize,
                             *raid.params.k, *raid.params.f, 2);
    }

    // read_at_multi/write_at_multi with a large configuration
    test write_read_two_stripes_jumbo(raid((41, 19, 3, 2))) {
        write_read_n_stripes(raid.val.0, *raid.params.chunksize,
                             *raid.params.k, *raid.params.f, 2);
    }

    // Write at least three rows to the layout.  Writing three rows guarantees
    // that some disks will have two data chunks separated by one parity chunk,
    // which tests the ability of VdevRaid::read_at to split a single disk's
    // data up into multiple VdevBlock::readv_at calls.
    test write_read_three_rows(raid) {
        let rows = 3;
        let stripes = div_roundup((rows * *raid.params.n) as usize,
                                   *raid.params.k as usize);
        write_read_n_stripes(raid.val.0, *raid.params.chunksize,
                             *raid.params.k, *raid.params.f, stripes);
    }

    test write_completes_a_partial_stripe(raid((3, 3, 1, 2))) {
        let (dbsw, dbsr) = make_bufs(*raid.params.chunksize, *raid.params.k,
                                     *raid.params.f, 1);
        let wbuf = dbsw.try().unwrap();
        let mut wbuf_l = wbuf.clone();
        let wbuf_r = wbuf_l.split_off(BYTES_PER_LBA);
        write_read0(raid.val.0, vec![wbuf_l, wbuf_r],
                    vec![dbsr.try_mut().unwrap()]);
        assert_eq!(wbuf, dbsr.try().unwrap());
    }

    test write_completes_a_partial_stripe_and_writes_a_bit_more(raid((3, 3, 1, 2))) {
        let (dbsw, dbsr) = make_bufs(*raid.params.chunksize, *raid.params.k,
                                     *raid.params.f, 2);
        {
            // Truncate buffers to be < 2 stripes' length
            let mut dbwm = dbsw.try_mut().unwrap();
            let dbwm_len = dbwm.len();
            dbwm.try_truncate(dbwm_len - BYTES_PER_LBA).expect("truncate");
            let mut dbrm = dbsr.try_mut().unwrap();
            dbrm.try_truncate(dbwm_len - BYTES_PER_LBA).expect("truncate");
        }
        {
            let mut wbuf_l = dbsw.try().unwrap();
            let wbuf_r = wbuf_l.split_off(BYTES_PER_LBA);
            let rbuf = dbsr.try_mut().unwrap();
            write_read0(raid.val.0, vec![wbuf_l, wbuf_r], vec![rbuf]);
        }
        assert_eq!(&dbsw.try().unwrap()[..], &dbsr.try().unwrap()[..]);
    }

    test write_completes_a_partial_stripe_and_writes_another(raid((3, 3, 1, 2))) {
        let (dbsw, dbsr) = make_bufs(*raid.params.chunksize, *raid.params.k,
                                     *raid.params.f, 2);
        let wbuf = dbsw.try().unwrap();
        let mut wbuf_l = wbuf.clone();
        let wbuf_r = wbuf_l.split_off(BYTES_PER_LBA);
        write_read0(raid.val.0, vec![wbuf_l, wbuf_r],
                    vec![dbsr.try_mut().unwrap()]);
        assert_eq!(wbuf, dbsr.try().unwrap());
    }

    test write_completes_a_partial_stripe_and_writes_two_more(raid((3, 3, 1, 2))) {
        let (dbsw, dbsr) = make_bufs(*raid.params.chunksize, *raid.params.k,
                                     *raid.params.f, 3);
        let wbuf = dbsw.try().unwrap();
        let mut wbuf_l = wbuf.clone();
        let wbuf_r = wbuf_l.split_off(BYTES_PER_LBA);
        write_read0(raid.val.0, vec![wbuf_l, wbuf_r],
                    vec![dbsr.try_mut().unwrap()]);
        assert_eq!(wbuf, dbsr.try().unwrap());
    }

    test write_completes_a_partial_stripe_and_writes_two_more_with_leftovers(raid((3, 3, 1, 2))) {
        let (dbsw, dbsr) = make_bufs(*raid.params.chunksize, *raid.params.k,
                                     *raid.params.f, 4);
        {
            // Truncate buffers to be < 4 stripes' length
            let mut dbwm = dbsw.try_mut().unwrap();
            let dbwm_len = dbwm.len();
            dbwm.try_truncate(dbwm_len - BYTES_PER_LBA).expect("truncate");
            let mut dbrm = dbsr.try_mut().unwrap();
            dbrm.try_truncate(dbwm_len - BYTES_PER_LBA).expect("truncate");
        }
        {
            let mut wbuf_l = dbsw.try().unwrap();
            let wbuf_r = wbuf_l.split_off(BYTES_PER_LBA);
            let rbuf = dbsr.try_mut().unwrap();
            write_read0(raid.val.0, vec![wbuf_l, wbuf_r], vec![rbuf]);
        }
        assert_eq!(&dbsw.try().unwrap()[..], &dbsr.try().unwrap()[..]);
    }

    test write_label(raid((5, 3, 1, 2))) {
        current_thread::block_on_all(future::lazy(|| {
            raid.val.0.write_label()
        })).unwrap();
        for path in raid.val.2 {
            let mut f = fs::File::open(path).unwrap();
            let mut v = vec![0; 8192];
            f.seek(SeekFrom::Start(4096)).unwrap();   // Skip the VdevLeaf label
            f.read_exact(&mut v).unwrap();
            // Compare against the golden master, skipping the checksum and UUID
            // fields
            assert_eq!(&v[0..27], &GOLDEN_LABEL[0..27]);
            assert_eq!(&v[35..47], &GOLDEN_LABEL[35..47]);
            assert_eq!(&v[63..138], &GOLDEN_LABEL[63..138]);
            // Rest of the buffer should be zero-filled
            assert!(v[223..].iter().all(|&x| x == 0));
        }
    }

    test write_partial_at_start_of_stripe(raid((3, 3, 1, 2))) {
        let (dbsw, dbsr) = make_bufs(*raid.params.chunksize, *raid.params.k,
                                     *raid.params.f, 1);
        let wbuf = dbsw.try().unwrap();
        let wbuf_short = wbuf.slice_to(BYTES_PER_LBA);
        {
            let mut rbuf = dbsr.try_mut().unwrap();
            let rbuf_short = rbuf.split_to(BYTES_PER_LBA);
            write_read0(raid.val.0, vec![wbuf_short], vec![rbuf_short]);
        }
        assert_eq!(&wbuf[0..BYTES_PER_LBA],
                   &dbsr.try().unwrap()[0..BYTES_PER_LBA]);
    }

    // Test that write_at works when directed at the middle of the StripeBuffer.
    // This test requires a chunksize > 2
    test write_partial_at_middle_of_stripe(raid((3, 3, 1, 16))) {
        let (dbsw, dbsr) = make_bufs(*raid.params.chunksize, *raid.params.k,
                                     *raid.params.f, 1);
        let wbuf = dbsw.try().unwrap().slice_to(2 * BYTES_PER_LBA);
        let wbuf_begin = wbuf.slice_to(BYTES_PER_LBA);
        let wbuf_middle = wbuf.slice_from(BYTES_PER_LBA);
        {
            let mut rbuf = dbsr.try_mut().unwrap();
            let _ = rbuf.split_off(2 * BYTES_PER_LBA);
            write_read0(raid.val.0, vec![wbuf_begin, wbuf_middle], vec![rbuf]);
        }
        assert_eq!(&wbuf[..],
                   &dbsr.try().unwrap()[0..2 * BYTES_PER_LBA],
                   "{:#?}\n{:#?}", &wbuf[..],
                   &dbsr.try().unwrap()[0..2 * BYTES_PER_LBA]);
    }

    test write_two_stripes_with_leftovers(raid((3, 3, 1, 2))) {
        let (dbsw, dbsr) = make_bufs(*raid.params.chunksize, *raid.params.k,
                                     *raid.params.f, 3);
        {
            // Truncate buffers to be < 3 stripes' length
            let mut dbwm = dbsw.try_mut().unwrap();
            let dbwm_len = dbwm.len();
            dbwm.try_truncate(dbwm_len - BYTES_PER_LBA).expect("truncate");
            let mut dbrm = dbsr.try_mut().unwrap();
            dbrm.try_truncate(dbwm_len - BYTES_PER_LBA).expect("truncate");
        }
        {
            let wbuf = dbsw.try().unwrap();
            let rbuf = dbsr.try_mut().unwrap();
            write_read0(raid.val.0, vec![wbuf], vec![rbuf]);
        }
        assert_eq!(&dbsw.try().unwrap()[..], &dbsr.try().unwrap()[..]);
    }

    test writev_read_one_stripe(raid) {
        writev_read_n_stripes(&raid.val.0, *raid.params.chunksize,
                              *raid.params.k, *raid.params.f, 1);
    }

    test zone_read_closed(raid((3, 3, 1, 2))) {
        let zone = 0;
        let zl = raid.val.0.zone_limits(zone);
        let (dbsw, dbsr) = make_bufs(*raid.params.chunksize, *raid.params.k,
                                     *raid.params.f, 1);
        let wbuf0 = dbsw.try().unwrap();
        let wbuf1 = dbsw.try().unwrap();
        let rbuf = dbsr.try_mut().unwrap();
        current_thread::block_on_all(future::lazy(|| {
            raid.val.0.write_at(wbuf0, zone, zl.0)
                .and_then(|_| {
                    raid.val.0.finish_zone(zone)
                }).and_then(|_| {
                    raid.val.0.read_at(rbuf, zl.0)
                })
        })).expect("current_thread::block_on_all");
        assert_eq!(wbuf1, dbsr.try().unwrap());
    }

    // Close a zone with an incomplete StripeBuffer, then read back from it
    test zone_read_closed_partial(raid((3, 3, 1, 2))) {
        let zone = 0;
        let zl = raid.val.0.zone_limits(zone);
        let (dbsw, dbsr) = make_bufs(*raid.params.chunksize, *raid.params.k,
                                     *raid.params.f, 1);
        let wbuf = dbsw.try().unwrap();
        let wbuf_short = wbuf.slice_to(BYTES_PER_LBA);
        {
            let mut rbuf = dbsr.try_mut().unwrap();
            let rbuf_short = rbuf.split_to(BYTES_PER_LBA);
            current_thread::block_on_all(future::lazy(|| {
                raid.val.0.write_at(wbuf_short, zone, zl.0)
                    .and_then(|_| {
                        raid.val.0.finish_zone(zone)
                    }).and_then(|_| {
                        raid.val.0.read_at(rbuf_short, zl.0)
                    })
            })).expect("current_thread::block_on_all");
        }
        assert_eq!(&wbuf[0..BYTES_PER_LBA],
                   &dbsr.try().unwrap()[0..BYTES_PER_LBA]);
    }


    #[should_panic]
    // Writing to an explicitly closed a zone fails
    test zone_close(raid((3, 3, 1, 2))) {
        let zone = 1;
        let (start, _) = raid.val.0.zone_limits(zone);
        let (dbsw, dbsr) = make_bufs(*raid.params.chunksize, *raid.params.k,
                                     *raid.params.f, 1);
        let wbuf0 = dbsw.try().unwrap();
        let wbuf1 = dbsw.try().unwrap();
        let rbuf = dbsr.try_mut().unwrap();
        current_thread::block_on_all(
            raid.val.0.open_zone(zone)
            .and_then(|_| raid.val.0.finish_zone(zone))
        ).expect("open and finish");
        write_read(&raid.val.0, vec![wbuf0], vec![rbuf], zone, start);
        assert_eq!(wbuf1, dbsr.try().unwrap());
    }

    #[should_panic]
    // Writing to a closed zone should fail
    test zone_write_closed(raid((3, 3, 1, 2))) {
        let zone = 1;
        let (start, _) = raid.val.0.zone_limits(zone);
        let dbsw = DivBufShared::from(vec![0;4096]);
        let wbuf = dbsw.try().unwrap();
        raid.val.0.write_at(wbuf, zone, start);
    }

    // Opening a closed zone should allow writing
    test zone_write_open(raid((3, 3, 1, 2))) {
        let zone = 1;
        let (start, _) = raid.val.0.zone_limits(zone);
        let (dbsw, dbsr) = make_bufs(*raid.params.chunksize, *raid.params.k,
                                     *raid.params.f, 1);
        let wbuf0 = dbsw.try().unwrap();
        let wbuf1 = dbsw.try().unwrap();
        let rbuf = dbsr.try_mut().unwrap();
        current_thread::block_on_all(
            raid.val.0.open_zone(zone)
        ).expect("open_zone");
        write_read(&raid.val.0, vec![wbuf0], vec![rbuf], zone, start);
        assert_eq!(wbuf1, dbsr.try().unwrap());
    }

    // Two zones can be open simultaneously
    test zone_write_two_zones(raid((3, 3, 1, 2))) {
        let vdev_raid = raid.val.0;
        for zone in 1..3 {
            let (start, _) = vdev_raid.zone_limits(zone);
            let (dbsw, dbsr) = make_bufs(*raid.params.chunksize, *raid.params.k,
                                         *raid.params.f, 1);
            let wbuf0 = dbsw.try().unwrap();
            let wbuf1 = dbsw.try().unwrap();
            let rbuf = dbsr.try_mut().unwrap();
            current_thread::block_on_all(
                vdev_raid.open_zone(zone)
            ).expect("open_zone");
            write_read(&vdev_raid, vec![wbuf0], vec![rbuf], zone, start);
            assert_eq!(wbuf1, dbsr.try().unwrap());
        }
    }
}
