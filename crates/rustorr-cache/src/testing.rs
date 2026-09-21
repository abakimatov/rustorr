//! Fixtures shared by the crate's tests, and the contract every `PieceStore`
//! must satisfy. Each backend runs the same checks through
//! `store_contract_tests!`.

use rustorr_domain::{FileIndex, InfoHash};

use crate::{Error, PieceStore, TorrentLayout};

pub(crate) fn torrent(n: u8) -> InfoHash {
    InfoHash::from_bytes([n; 20])
}

pub(crate) fn file(n: u32) -> FileIndex {
    FileIndex::from_zero_based(n)
}

/// Two files, of 100 000 and 50 000 bytes.
pub(crate) fn layout() -> TorrentLayout {
    TorrentLayout::new(64 * 1024, vec![100_000, 50_000]).unwrap()
}

pub(crate) fn pattern(len: usize, seed: u8) -> Vec<u8> {
    (0..len)
        .map(|i| (i as u8).wrapping_mul(7).wrapping_add(seed))
        .collect()
}

fn read(store: &dyn PieceStore, t: u8, f: u32, offset: u64, len: usize) -> Result<Vec<u8>, Error> {
    let mut buf = vec![0xAA; len];
    store.read(torrent(t), file(f), offset, &mut buf)?;
    Ok(buf)
}

pub(crate) fn round_trips_across_block_boundaries(store: &dyn PieceStore) {
    store.open(torrent(1), &layout()).unwrap();
    let data = pattern(40_000, 1);
    let second = pattern(3_000, 9);

    assert_eq!(
        store.write(torrent(1), file(0), 16_000, &data).unwrap(),
        40_000
    );
    store.write(torrent(1), file(1), 0, &second).unwrap();

    assert_eq!(read(store, 1, 0, 16_000, 40_000).unwrap(), data);
    assert_eq!(
        read(store, 1, 0, 20_000, 5_000).unwrap(),
        data[4_000..9_000]
    );
    assert_eq!(read(store, 1, 1, 0, 3_000).unwrap(), second);
}

pub(crate) fn never_stored_bytes_are_an_error_not_zeros(store: &dyn PieceStore) {
    store.open(torrent(1), &layout()).unwrap();

    assert!(matches!(
        read(store, 1, 0, 0, 100),
        Err(Error::Missing { .. })
    ));

    store.write(torrent(1), file(0), 0, &[5; 50]).unwrap();
    assert!(
        matches!(read(store, 1, 0, 0, 100), Err(Error::Missing { .. })),
        "a range that is only partly stored must not be readable"
    );
    assert_eq!(read(store, 1, 0, 0, 50).unwrap(), vec![5; 50]);

    // Stored zeros are data, unlike bytes that were never received.
    store.write(torrent(1), file(0), 200, &[0; 64]).unwrap();
    assert_eq!(read(store, 1, 0, 200, 64).unwrap(), vec![0; 64]);
}

pub(crate) fn write_reports_only_new_bytes(store: &dyn PieceStore) {
    store.open(torrent(1), &layout()).unwrap();
    let write = |offset, len| {
        store
            .write(torrent(1), file(0), offset, &vec![1; len])
            .unwrap()
    };

    assert_eq!(write(0, 100), 100);
    assert_eq!(write(0, 100), 0);
    assert_eq!(write(50, 100), 50);
    assert_eq!(write(200, 10), 10);
    assert_eq!(write(100, 100), 50);
}

pub(crate) fn rejects_bad_ranges_and_files(store: &dyn PieceStore) {
    store.open(torrent(1), &layout()).unwrap();

    assert!(matches!(
        store.write(torrent(1), file(0), 99_990, &[0; 20]),
        Err(Error::OutOfBounds {
            file_len: 100_000,
            ..
        })
    ));
    assert!(matches!(
        read(store, 1, 1, 49_990, 20),
        Err(Error::OutOfBounds {
            file_len: 50_000,
            ..
        })
    ));
    assert!(matches!(
        store.write(torrent(1), file(2), 0, &[0]),
        Err(Error::UnknownFile { .. })
    ));
    assert!(matches!(
        store.write(torrent(1), file(0), u64::MAX, &[0]),
        Err(Error::Domain(_))
    ));
}

pub(crate) fn unknown_torrent_is_an_error(store: &dyn PieceStore) {
    assert!(matches!(
        store.write(torrent(1), file(0), 0, &[0]),
        Err(Error::UnknownTorrent(_))
    ));
    assert!(matches!(
        read(store, 1, 0, 0, 1),
        Err(Error::UnknownTorrent(_))
    ));
}

pub(crate) fn remove_drops_only_that_torrent(store: &dyn PieceStore) {
    store.open(torrent(1), &layout()).unwrap();
    store.open(torrent(2), &layout()).unwrap();
    store.write(torrent(1), file(0), 0, &[1; 10]).unwrap();
    store.write(torrent(2), file(0), 0, &[2; 10]).unwrap();

    store.remove(torrent(1)).unwrap();

    assert!(matches!(
        read(store, 1, 0, 0, 10),
        Err(Error::UnknownTorrent(_))
    ));
    assert_eq!(read(store, 2, 0, 0, 10).unwrap(), vec![2; 10]);
    store.remove(torrent(1)).unwrap();
    store.remove(torrent(9)).unwrap();
}

pub(crate) fn open_discards_earlier_contents(store: &dyn PieceStore) {
    store.open(torrent(1), &layout()).unwrap();
    store.write(torrent(1), file(0), 0, &[3; 10]).unwrap();

    store.open(torrent(1), &layout()).unwrap();

    assert!(matches!(
        read(store, 1, 0, 0, 10),
        Err(Error::Missing { .. })
    ));
}

pub(crate) fn empty_transfers_are_harmless(store: &dyn PieceStore) {
    store.open(torrent(1), &layout()).unwrap();

    assert_eq!(store.write(torrent(1), file(1), 10, &[]).unwrap(), 0);
    assert_eq!(read(store, 1, 1, 10, 0).unwrap(), Vec::<u8>::new());
}

pub(crate) fn concurrent_writers_and_readers_see_their_own_bytes(store: &dyn PieceStore) {
    store.open(torrent(1), &layout()).unwrap();

    std::thread::scope(|scope| {
        for worker in 0..4u8 {
            scope.spawn(move || {
                let base = u64::from(worker) * 20_000;
                let data = pattern(20_000, worker);
                for (i, chunk) in data.chunks(625).enumerate() {
                    let offset = base + (i * 625) as u64;
                    store.write(torrent(1), file(0), offset, chunk).unwrap();
                    assert_eq!(read(store, 1, 0, offset, chunk.len()).unwrap(), chunk);
                }
                assert_eq!(read(store, 1, 0, base, 20_000).unwrap(), data);
            });
        }
    });
}

macro_rules! store_contract_tests {
    ($make:expr) => {
        $crate::testing::store_contract_tests!(@each $make,
            round_trips_across_block_boundaries,
            never_stored_bytes_are_an_error_not_zeros,
            write_reports_only_new_bytes,
            rejects_bad_ranges_and_files,
            unknown_torrent_is_an_error,
            remove_drops_only_that_torrent,
            open_discards_earlier_contents,
            empty_transfers_are_harmless,
            concurrent_writers_and_readers_see_their_own_bytes
        );
    };
    (@each $make:expr, $($check:ident),+) => {
        $(
            #[test]
            fn $check() {
                let (store, _keep) = $make;
                $crate::testing::$check(&store);
            }
        )+
    };
}
pub(crate) use store_contract_tests;
